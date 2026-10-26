//! `tilo-control` — drive a running Tilo desktop through its control sockets.
//!
//! Wraps the compositor's JSON control protocol (one request and one response
//! per line, defined by `shell::shell::test_control`) so scripts don't have to
//! glue `socat` to `jq`. Two sockets speak it:
//!
//! - the device admin's control socket,
//!   `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`, which every build
//!   serves in the device admin's session once first login is complete. It
//!   answers the action model — `actions`, `state-dump`, `windows` — and is
//!   where those subcommands go by default;
//! - the development test-control socket, bound only by development builds
//!   at `$TILO_TEST_CONTROL`, which also takes synthetic input and
//!   screenshots. Input subcommands go there (default
//!   `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`), and a set
//!   `$TILO_TEST_CONTROL` routes every subcommand there.
//!
//! `--socket PATH` overrides both. No `clap`, no async, just stdlib +
//! `serde_json`. Run `--help` for the subcommand list.
//!
//! Exit codes: `0` on `Ok`/`Targets`/`State`, `1` on `Err` (echoing
//! `err.code`/`err.message` to stderr), `2` on argv / I/O / parse failures.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use tilo_control::client::{connect, id_from_printed, invoke_wire, printable_id, round_trip};
use tilo_control::{TEST_CONTROL_ENV, admin_socket_path, default_socket_path};

/// Which socket a command was routed to, so a failure to reach it can say
/// what that socket is and how to get one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketKind {
    Named,
    Admin,
    /// The default development test-control socket.
    TestControl,
}

/// Pick the socket: `--socket`, else `$TILO_TEST_CONTROL`, else the default
/// in the private runtime directory — the admin control socket for the
/// action model, the test-control socket for synthetic input. Never a shared
/// directory: without a runtime directory the caller has to name the socket.
fn resolve_socket(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    runtime_dir: Option<OsString>,
    semantic: bool,
) -> Result<(PathBuf, SocketKind), ParseError> {
    if let Some(path) = flag.or_else(|| env.filter(|env| !env.is_empty()).map(PathBuf::from)) {
        return Ok((path, SocketKind::Named));
    }
    let default = if semantic {
        admin_socket_path(runtime_dir).map(|path| (path, SocketKind::Admin))
    } else {
        default_socket_path(runtime_dir).map(|path| (path, SocketKind::TestControl))
    };
    default.ok_or_else(|| {
        ParseError::Usage(format!(
            "no control socket: pass --socket PATH or set ${TEST_CONTROL_ENV} \
             (XDG_RUNTIME_DIR is unset or relative, so there is no default; a login \
             or ssh session as the desktop's own account sets it)"
        ))
    })
}

/// What to do when the socket cannot be reached: what it is, why it may be
/// missing, and the route that works.
fn connect_hint(kind: SocketKind, error: &std::io::Error) -> String {
    match (kind, error.kind()) {
        (SocketKind::Admin, std::io::ErrorKind::PermissionDenied) => {
            "the admin control socket belongs to another account; it serves only the account \
             whose session it is"
                .to_string()
        }
        (SocketKind::Admin, _) => "the desktop serves the admin control socket in the device \
             admin's session once first login is complete. Run this as the account that set \
             the device up (its own login or ssh), with the desktop signed in. A development \
             session is reached with TILO_TEST_CONTROL=<socket> instead."
            .to_string(),
        (SocketKind::TestControl, _) => format!(
            "synthetic input and screenshots exist only in a development session started with \
             ${TEST_CONTROL_ENV} (a dev-overrides build). To drive a desktop, use the action \
             model instead: `tilo-control actions list`, `actions invoke`, `windows`."
        ),
        (SocketKind::Named, _) => {
            "is the desktop running, and is this the socket it bound?".to_string()
        }
    }
}

/// What a person reads when the admin's socket turns their process away:
/// it serves only the device owner's own login, and a terminal app, with
/// everything run in it, is not that. `tilo action` reaches the desktop
/// from there, with the terminal workspace's scope.
const TURNED_AWAY: &str = "tilo-control works only from the device owner's own login.\n\
     In a terminal app, use `tilo action` instead:\n\
     \x20 tilo actions              what this desktop can do\n\
     \x20 tilo do <action>          do one of those things\n\
     \x20 tilo open <app>           open an app";

/// Whether the admin socket answered by turning this process away rather
/// than answering its request.
fn turned_away_by_admin_socket(kind: SocketKind, response: &serde_json::Value) -> bool {
    kind == SocketKind::Admin
        && tilo_control::client::refusal_of_process(
            &tilo_control::client::Door::Admin(PathBuf::new()),
            response,
        )
        .is_some()
}

/// A next step for a refusal the compositor answered with.
fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "unknown_action" => Some(
            "list what this desktop offers with `tilo-control actions list [QUERY]` and pass \
             back a PROVIDER ID pair exactly as listed",
        ),
        "invalid_arguments" => Some(
            "`tilo-control actions list QUERY --json` shows each action's `params`; pass them \
             as --args-json '{\"name\": value}'",
        ),
        "session_locked" => Some("unlock the session at the device, then retry"),
        "stale_cursor" => {
            Some("the actions changed while you paged; list again from the first page")
        }
        _ => None,
    }
}

/// Internal representation of a parsed CLI invocation. Built by
/// [`parse_args`]; consumed by `main` to produce the JSON command +
/// post-response printout.
#[derive(Debug, Clone, PartialEq)]
enum Command {
    ListTargets,
    Click {
        name: String,
    },
    ClickAt {
        x: f64,
        y: f64,
    },
    Drag {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        steps: Option<u32>,
    },
    Key {
        key: String,
    },
    Type {
        text: String,
    },
    Modifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
    },
    Snapshot {
        path: String,
    },
    /// A touchpad swipe; the shell takes three fingers or more.
    Swipe {
        fingers: u32,
        dx: f64,
        dy: f64,
        steps: Option<u32>,
    },
    /// A touchpad pinch to absolute `scale`.
    Pinch {
        fingers: u32,
        scale: f64,
        steps: Option<u32>,
    },
    /// Attach (`on`) or detach (`off`) a viewer of a headless session.
    Watch {
        watching: bool,
    },
    StateDump {
        pretty: bool,
        /// `--history`: keep the workspaces' append-only history (effect
        /// receipts, task audit trails), which the dump leaves out by default.
        history: bool,
    },
    /// The open windows in the desktop's own terms: workspace, strip, window
    /// group, tab, as the `windows` op answers them.
    Windows {
        json: bool,
    },
    /// List the actions the shell exports right now. Read-only.
    ActionsList {
        query: String,
        json: bool,
        include_programmatic: bool,
        page: ListPage,
        /// `--wait[=MS]`: answer once the listing is complete.
        wait_ms: Option<u64>,
    },
    /// Invoke one enumerated action by its stable `(provider, id)`
    /// address — no synthetic input, no window, no scene.
    ActionsInvoke {
        provider: String,
        id: String,
        args_json: Option<String>,
        args_stdin: bool,
        device: Option<String>,
        /// `--wait[=MS]`: answer once the invocation settles, waiting at
        /// most this long, rather than once it is admitted.
        wait_ms: Option<u64>,
    },
    ActionsStatus {
        invocation_id: u64,
        /// `--wait[=MS]`: answer once the invocation has settled.
        wait_ms: Option<u64>,
    },
    ActionsCancel {
        invocation_id: u64,
    },
    /// One connection for many requests: one wire request per stdin line,
    /// one answer per stdout line.
    ActionsSession,
    Raw {
        json: String,
    },
    /// Read what an open app holds through its `read` action: the resource
    /// itself, or the nested `item` it lists.
    Read {
        app: String,
        item: Option<String>,
        json: bool,
    },
}

impl Command {
    /// Whether the command belongs to the action model, which the device
    /// admin's control socket serves; everything else is synthetic input or
    /// capture, which only a development session's test-control socket takes.
    const fn is_semantic(&self) -> bool {
        matches!(
            self,
            Self::StateDump { .. }
                | Self::Windows { .. }
                | Self::ActionsList { .. }
                | Self::ActionsInvoke { .. }
                | Self::ActionsStatus { .. }
                | Self::ActionsCancel { .. }
                | Self::ActionsSession
                | Self::Raw { .. }
                | Self::Read { .. }
        )
    }
}

/// Which page of `actions list` to ask for: `--limit ROWS` and `--cursor
/// NEXT`, the `next_cursor` the previous page answered. Neither means the
/// desktop's default first page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ListPage {
    limit: Option<usize>,
    cursor: Option<String>,
    /// `--kind KIND`, repeatable: only rows of these kinds (`application`,
    /// `action`, `note`, `file`, …).
    kinds: Vec<String>,
    /// `--files`: also list files and folders, which a listing leaves out
    /// unless asked: every checkout's `markdown.rs` is not an app's action.
    files: bool,
}

/// Top-level CLI invocation — the `--socket` override, if given, + the
/// parsed subcommand. The effective path is settled by [`resolve_socket`].
#[derive(Debug, Clone, PartialEq)]
struct Invocation {
    socket: Option<PathBuf>,
    command: Command,
}

/// Argv parse error; the `(code, message)` shape mirrors the wire's
/// `Response::Err`.
#[derive(Debug, Clone, PartialEq)]
enum ParseError {
    /// Caller asked for help — print usage and exit 0.
    Help,
    /// Invalid input — print message to stderr and exit 2.
    Usage(String),
}

/// Parse `argv` (without the program name) into an [`Invocation`]. Accepts
/// `--socket PATH` anywhere before the subcommand; subcommand arguments are
/// positional and required, with optional trailing args matched per-subcommand.
/// Scan `argv` for `--help`/`-h` and `--socket PATH`, which may appear
/// anywhere before the subcommand; everything else is returned as
/// positional args. Split out of [`parse_args`] to keep it short.
fn parse_global_flags(argv: &[String]) -> Result<(Option<PathBuf>, Vec<String>), ParseError> {
    let mut socket: Option<PathBuf> = None;
    let mut positional: Vec<String> = Vec::with_capacity(argv.len());
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => return Err(ParseError::Help),
            "--socket" => {
                let val = iter.next().ok_or_else(|| {
                    ParseError::Usage("--socket requires a path argument".to_string())
                })?;
                socket = Some(PathBuf::from(val));
            }
            other => positional.push(other.to_string()),
        }
    }
    Ok((socket, positional))
}

fn parse_watch(arg: Option<&String>) -> Result<Command, ParseError> {
    match arg.map(String::as_str) {
        Some("on") => Ok(Command::Watch { watching: true }),
        Some("off") => Ok(Command::Watch { watching: false }),
        _ => Err(ParseError::Usage("watch requires on or off".to_string())),
    }
}

fn parse_args(argv: &[String]) -> Result<Invocation, ParseError> {
    let (socket, positional) = parse_global_flags(argv)?;
    let mut iter = positional.iter();
    let op = iter
        .next()
        .ok_or_else(|| ParseError::Usage("missing subcommand".to_string()))?;

    let command = match op.as_str() {
        "list-targets" => Command::ListTargets,
        "click" => {
            let name = iter
                .next()
                .ok_or_else(|| ParseError::Usage("click NAME requires a name".to_string()))?;
            Command::Click { name: name.clone() }
        }
        "click-at" => {
            let x = next_f64(&mut iter, "click-at X Y requires X")?;
            let y = next_f64(&mut iter, "click-at X Y requires Y")?;
            Command::ClickAt { x, y }
        }
        "drag" => {
            let x1 = next_f64(&mut iter, "drag X1 Y1 X2 Y2 [STEPS]")?;
            let y1 = next_f64(&mut iter, "drag X1 Y1 X2 Y2 [STEPS]")?;
            let x2 = next_f64(&mut iter, "drag X1 Y1 X2 Y2 [STEPS]")?;
            let y2 = next_f64(&mut iter, "drag X1 Y1 X2 Y2 [STEPS]")?;
            let steps = iter
                .next()
                .map(|s| {
                    s.parse::<u32>()
                        .map_err(|e| ParseError::Usage(format!("drag STEPS must be u32 ({e})")))
                })
                .transpose()?;
            Command::Drag {
                x1,
                y1,
                x2,
                y2,
                steps,
            }
        }
        "swipe" | "pinch" => parse_gesture(op, &mut iter)?,
        "key" => {
            let key = iter
                .next()
                .ok_or_else(|| ParseError::Usage("key KEY requires a key name".to_string()))?;
            Command::Key { key: key.clone() }
        }
        "type" => {
            let text = iter
                .next()
                .ok_or_else(|| ParseError::Usage("type TEXT requires text".to_string()))?;
            Command::Type { text: text.clone() }
        }
        "modifiers" => {
            let flags = iter.next().map_or("", String::as_str);
            let (ctrl, shift, alt, logo) = parse_modifier_flags(flags)?;
            Command::Modifiers {
                ctrl,
                shift,
                alt,
                logo,
            }
        }
        "snapshot" => {
            let path = iter
                .next()
                .ok_or_else(|| ParseError::Usage("snapshot PATH requires a path".to_string()))?;
            Command::Snapshot { path: path.clone() }
        }
        "watch" => parse_watch(iter.next())?,
        "state-dump" | "windows" => parse_state_read(op, &mut iter)?,
        "actions" => parse_actions_command(&mut iter)?,
        "read" => parse_read(&mut iter)?,
        "raw" => {
            let json = iter.next().ok_or_else(|| {
                ParseError::Usage("raw '<json>' requires the JSON payload".to_string())
            })?;
            Command::Raw { json: json.clone() }
        }
        other => {
            return Err(ParseError::Usage(format!(
                "unknown subcommand `{other}` (try --help)"
            )));
        }
    };

    Ok(Invocation { socket, command })
}

/// Parse `read APP [ITEM] [--json]`.
fn parse_read<'a>(iter: &mut impl Iterator<Item = &'a String>) -> Result<Command, ParseError> {
    let mut json = false;
    let mut words = Vec::new();
    for rest in iter.by_ref() {
        if rest == "--json" {
            json = true;
        } else {
            words.push(rest.clone());
        }
    }
    let mut words = words.into_iter();
    let app = words.next().ok_or_else(|| {
        ParseError::Usage("read APP [ITEM] requires an app id, such as so.tilo.notes".to_string())
    })?;
    let item = words.next();
    if words.next().is_some() {
        return Err(ParseError::Usage(
            "read takes an app id and at most one item id".to_string(),
        ));
    }
    Ok(Command::Read { app, item, json })
}

/// How long `read` waits for an open app's `read` to be listed: the listing
/// settles as the desktop indexes it, and an app just opened publishes its
/// actions as it starts. The desktop holds each listing request until the
/// listing has settled or changed (`wait_ms`), so this is a deadline, never a
/// pause.
const READ_DISCOVERY_WAIT_MS: u64 = 10_000;
/// The most listings `read` looks at. Each one is a changed listing the
/// desktop waited for, so this bounds a misbehaving desktop, not a wait.
const READ_DISCOVERY_LISTINGS: u32 = 64;
/// How long `read` follows an admitted invocation to its answer: an app
/// action is admitted first and answers on its job. The desktop holds the
/// reply until then (`wait_ms`).
const READ_ANSWER_WAIT_MS: u64 = 20_000;

/// Whether an invocation in `state` has ended, one way or another.
fn settled(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled")
}

/// The address of `app`'s `read` action in an enumeration answer.
fn read_action_id(answer: &serde_json::Value, app: &str) -> Option<String> {
    answer
        .get("actions")?
        .as_array()?
        .iter()
        .find(|row| {
            row.get("provider").and_then(serde_json::Value::as_str) == Some("app-actions")
                && row.get("application").and_then(serde_json::Value::as_str) == Some(app)
                && row.get("operation").and_then(serde_json::Value::as_str) == Some("read")
        })
        .and_then(|row| row.get("id")?.as_str().map(str::to_string))
}

/// The `read` listing request: settled within `wait_ms`, and with
/// `after_revision`, a listing other than the one already seen.
fn read_listing_request(wait_ms: u64, after_revision: Option<&str>) -> std::io::Result<String> {
    let request: tilo_control::protocol::Command =
        tilo_control::protocol::Command::ActionsEnumerate {
            query: "read".into(),
            include_programmatic: false,
            page: Default::default(),
            wait_ms: Some(wait_ms),
            after_revision: after_revision.map(str::to_owned),
        };
    serde_json::to_string(&request)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Milliseconds of `budget` left since `started`.
fn remaining_ms(budget: u64, started: std::time::Instant) -> u64 {
    let spent = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    budget.saturating_sub(spent)
}

/// `read APP [ITEM]` over one connection: find the open app's `read` action,
/// invoke it, and print the typed value it answers.
fn run_read(stream: &mut UnixStream, app: &str, item: Option<&str>, json: bool) -> u8 {
    run_read_with(&mut |request| round_trip(stream, request), app, item, json)
}

/// [`run_read`] over any request/answer transport. Nothing here pauses:
/// every request asks the desktop to hold its answer (`wait_ms`) until there
/// is one worth giving — a settled listing, a changed listing, the app's
/// answer — so it usually takes two requests, one listing and one invocation.
fn run_read_with(
    round_trip: &mut dyn FnMut(&str) -> std::io::Result<serde_json::Value>,
    app: &str,
    item: Option<&str>,
    json: bool,
) -> u8 {
    let started = std::time::Instant::now();
    let mut seen: Option<String> = None;
    let mut id = None;
    for _ in 0..READ_DISCOVERY_LISTINGS {
        let wait_ms = remaining_ms(READ_DISCOVERY_WAIT_MS, started);
        let response = match read_listing_request(wait_ms, seen.as_deref())
            .and_then(|request| round_trip(&request))
        {
            Ok(response) => response,
            Err(e) => {
                eprintln!("tilo-control: socket round-trip: {e}");
                return 2;
            }
        };
        if response.get("err").is_some() {
            return print_response(
                &Command::ActionsList {
                    include_programmatic: false,
                    query: "read".into(),
                    json,
                    page: ListPage::default(),
                    wait_ms: None,
                },
                &response,
            );
        }
        let answer = response.get("answer");
        id = answer.and_then(|answer| read_action_id(answer, app));
        if id.is_some() {
            break;
        }
        // Ask again only a desktop that waits — it holds the next request
        // until the listing changes — and only while time is left. One that
        // answers without `settled` cannot be asked for "the next listing",
        // and asking anyway would spin.
        let revision = answer
            .filter(|answer| answer.get("settled").is_some())
            .and_then(|answer| answer.get("revision")?.as_str());
        match revision {
            Some(revision) if remaining_ms(READ_DISCOVERY_WAIT_MS, started) > 0 => {
                seen = Some(revision.to_string());
            }
            _ => break,
        }
    }
    let Some(id) = id else {
        eprintln!("error [unknown_action]: no open `{app}` offers `read`");
        eprintln!(
            "hint: open it with `tilo-control actions invoke applications {app}`; \
             `tilo-control actions list read` shows what the desktop can read now"
        );
        return 1;
    };
    let mut args = serde_json::Map::new();
    if let Some(item) = item {
        args.insert("item".into(), item.into());
    }
    let invoke = serde_json::json!({
        "op": "actions_invoke",
        "provider": "app-actions",
        "id": id,
        "args": args,
        "wait_ms": READ_ANSWER_WAIT_MS,
    })
    .to_string();
    let response = match round_trip(&invoke) {
        Ok(response) => response,
        Err(e) => {
            eprintln!("tilo-control: socket round-trip: {e}");
            return 2;
        }
    };
    print_read_answer(&response, json)
}

/// `actions invoke --wait`: print the answer as without `--wait`, then say
/// how the action ended. One that failed or was cancelled exits 1 with its
/// reason; one still running when the wait ran out says how to follow it.
fn print_settled(response: &serde_json::Value) -> u8 {
    let answer = response
        .get("answer")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    println!("{answer}");
    let update = answer
        .pointer("/receipt/update")
        .cloned()
        .unwrap_or_default();
    let text = |pointer: &str| {
        update
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    match text("/state").as_deref() {
        Some("failed") => {
            let code = text("/error/code").unwrap_or_else(|| "action_failed".into());
            eprintln!(
                "error [{code}]: {}",
                text("/error/message").unwrap_or_default()
            );
            1
        }
        Some("cancelled") => {
            eprintln!(
                "error [cancelled]: {}",
                text("/message").unwrap_or_default()
            );
            1
        }
        Some(state) if !settled(state) => {
            if let Some(job) = answer
                .pointer("/receipt/broker_invocation_id")
                .and_then(serde_json::Value::as_u64)
            {
                eprintln!(
                    "hint: still {state} when the wait ran out; follow it with \
                     `tilo-control actions status {job}`"
                );
            }
            0
        }
        _ => 0,
    }
}

/// `actions session`: send each request line of `requests` over the one
/// connection and write each answer line to `answers`, in order, until
/// `requests` ends. Blank lines are skipped. One process and one connection
/// serve every call, instead of one of each per call.
fn run_session(
    stream: &UnixStream,
    requests: impl BufRead,
    mut answers: impl Write,
) -> std::io::Result<()> {
    let mut sender = stream;
    let mut replies = BufReader::new(stream);
    for request in requests.lines() {
        let request = request?;
        let request = request.trim();
        if request.is_empty() {
            continue;
        }
        sender.write_all(request.as_bytes())?;
        sender.write_all(b"\n")?;
        let mut reply = String::new();
        if replies.read_line(&mut reply)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the desktop closed the connection",
            ));
        }
        answers.write_all(reply.as_bytes())?;
        answers.flush()?;
    }
    Ok(())
}

/// Print the value a `read` answered, or why it did not.
fn print_read_answer(response: &serde_json::Value, json: bool) -> u8 {
    if let Some(err) = response.get("err") {
        let code = err
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("error [{code}]: {msg}");
        return 1;
    }
    let update = response
        .pointer("/answer/receipt/update")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match update.get("state").and_then(serde_json::Value::as_str) {
        Some("succeeded") => {
            let value = update
                .pointer("/output/value")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let out = if json {
                value.to_string()
            } else {
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            };
            println!("{out}");
            0
        }
        Some("failed") => {
            let error = update.get("error").cloned().unwrap_or_default();
            let code = error
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("action_failed");
            let msg = error.get("message").and_then(|v| v.as_str()).unwrap_or("");
            eprintln!("error [{code}]: {msg}");
            1
        }
        state => {
            // A receipt is not what the resource holds: printing it as the
            // answer would hand the caller something else to parse.
            eprintln!(
                "error [unanswered]: the read did not answer (state {}): {}",
                state.unwrap_or("none"),
                response.get("answer").cloned().unwrap_or_default()
            );
            1
        }
    }
}

/// Parse `state-dump [--pretty] [--history]` and `windows [--json]`, the
/// two reads of the session's state.
fn parse_state_read<'a>(
    op: &str,
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let flags: &[&str] = if op == "windows" {
        &["--json"]
    } else {
        &["--pretty", "--history"]
    };
    let mut set = [false; 2];
    for rest in iter.by_ref() {
        let Some(index) = flags.iter().position(|flag| rest == flag) else {
            return Err(ParseError::Usage(format!(
                "{op}: unknown flag `{rest}` (only {})",
                flags.join(", ")
            )));
        };
        if let Some(slot) = set.get_mut(index) {
            *slot = true;
        }
    }
    let [first, history] = set;
    Ok(if op == "windows" {
        Command::Windows { json: first }
    } else {
        Command::StateDump {
            pretty: first,
            history,
        }
    })
}

/// Parse the `actions list|invoke ...` subcommand. Split out of
/// [`parse_args`] to keep it short.
fn parse_gesture<'a>(
    op: &str,
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    const SWIPE: &str = "swipe FINGERS DX DY [STEPS]";
    const PINCH: &str = "pinch FINGERS SCALE [STEPS]";
    if op == "swipe" {
        let fingers = next_u32(iter, SWIPE)?;
        let dx = next_f64(iter, SWIPE)?;
        let dy = next_f64(iter, SWIPE)?;
        let steps = optional_u32(iter, SWIPE)?;
        return Ok(Command::Swipe {
            fingers,
            dx,
            dy,
            steps,
        });
    }
    let fingers = next_u32(iter, PINCH)?;
    let scale = next_f64(iter, PINCH)?;
    let steps = optional_u32(iter, PINCH)?;
    Ok(Command::Pinch {
        fingers,
        scale,
        steps,
    })
}

fn parse_action_invocation<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let provider = iter.next().ok_or_else(|| {
        ParseError::Usage("actions invoke PROVIDER ID requires a provider".to_string())
    })?;
    let id = iter.next().ok_or_else(|| {
        ParseError::Usage("actions invoke PROVIDER ID requires an id".to_string())
    })?;
    let mut args_json = None;
    let mut args_stdin = false;
    let mut device = None;
    // An invocation answers its result: the receipt of an admission says
    // nothing a caller can act on, and a second `actions status` to learn
    // how it went is the polling this default removes.
    let mut wait_ms = Some(DEFAULT_INVOKE_WAIT_MS);
    let mut waited = false;
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--wait" => {
                wait_ms = Some(DEFAULT_INVOKE_WAIT_MS);
                waited = true;
            }
            "--no-wait" => {
                wait_ms = None;
                waited = false;
            }
            "--device" => {
                let id = iter.next().ok_or_else(|| {
                    ParseError::Usage("--device requires an enrolled device id".to_string())
                })?;
                device = Some(id.clone());
            }
            "--args-stdin" => {
                if args_stdin || args_json.is_some() {
                    return Err(ParseError::Usage(
                        "action arguments require exactly one source".into(),
                    ));
                }
                args_stdin = true;
            }
            "--args-json" => {
                if args_stdin || args_json.is_some() {
                    return Err(ParseError::Usage(
                        "action arguments require exactly one source".into(),
                    ));
                }
                let json = iter.next().ok_or_else(|| {
                    ParseError::Usage("--args-json requires a JSON object".to_string())
                })?;
                let parsed: serde_json::Value = serde_json::from_str(json).map_err(|error| {
                    ParseError::Usage(format!("--args-json is not valid JSON: {error}"))
                })?;
                if !parsed.is_object() {
                    return Err(ParseError::Usage(
                        "--args-json must contain a JSON object".to_string(),
                    ));
                }
                args_json = Some(json.clone());
            }
            other => {
                let Some(ms) = other.strip_prefix("--wait=") else {
                    return Err(ParseError::Usage(format!(
                        "actions invoke: unknown option `{other}`"
                    )));
                };
                wait_ms = Some(ms.parse::<u64>().map_err(|_| {
                    ParseError::Usage(format!("--wait=MS: `{ms}` is not a number of milliseconds"))
                })?);
                waited = true;
            }
        }
    }
    if device.is_some() && !waited {
        // One sent to a device answers once the device has it.
        wait_ms = None;
    }
    if wait_ms.is_some() && device.is_some() {
        return Err(ParseError::Usage(
            "--wait follows an invocation run here; one sent with --device answers once \
             the device has it"
                .into(),
        ));
    }
    Ok(Command::ActionsInvoke {
        provider: id_from_printed(provider),
        id: id_from_printed(id),
        args_json,
        args_stdin,
        device,
        wait_ms,
    })
}

/// How long `actions invoke --wait` waits for the invocation to settle when
/// no time is given: the MCP door's default. The desktop caps any wait at
/// 30 s.
const DEFAULT_INVOKE_WAIT_MS: u64 = 5_000;

/// `--wait` (the default wait) or `--wait=MS`; `None` for any other word.
fn wait_flag(word: &str) -> Option<Result<u64, ParseError>> {
    if word == "--wait" {
        return Some(Ok(DEFAULT_INVOKE_WAIT_MS));
    }
    let ms = word.strip_prefix("--wait=")?;
    Some(ms.parse::<u64>().map_err(|_| {
        ParseError::Usage(format!("--wait=MS: `{ms}` is not a number of milliseconds"))
    }))
}

/// `actions list [flags] [query words]`.
fn parse_actions_list<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    // Everything after `list` that isn't a flag joins the
    // query, so `actions list open settings` works without
    // the caller having to quote it.
    let mut json = false;
    let mut include_programmatic = false;
    let mut page = ListPage::default();
    let mut wait_ms = None;
    let mut words: Vec<String> = Vec::new();
    while let Some(rest) = iter.next() {
        if rest == "--json" {
            json = true;
        } else if rest == "--programmatic" {
            include_programmatic = true;
        } else if rest == "--limit" {
            let raw = iter.next().ok_or_else(|| {
                ParseError::Usage("actions list --limit needs a number of rows".into())
            })?;
            page.limit = Some(raw.parse::<usize>().map_err(|_| {
                ParseError::Usage(format!(
                    "actions list --limit: `{raw}` is not a number of rows"
                ))
            })?);
        } else if rest == "--files" {
            page.files = true;
        } else if rest == "--kind" {
            page.kinds.push(
                iter.next()
                    .ok_or_else(|| {
                        ParseError::Usage(
                            "actions list --kind needs a kind: application, action, \
                             note, record, file, …"
                                .into(),
                        )
                    })?
                    .clone(),
            );
        } else if rest == "--cursor" {
            page.cursor = Some(
                iter.next()
                    .ok_or_else(|| {
                        ParseError::Usage(
                            "actions list --cursor needs the next_cursor a page gave".into(),
                        )
                    })?
                    .clone(),
            );
        } else if let Some(wait) = wait_flag(rest) {
            wait_ms = Some(wait?);
        } else {
            words.push(rest.clone());
        }
    }
    Ok(Command::ActionsList {
        query: words.join(" "),
        json,
        include_programmatic,
        page,
        wait_ms,
    })
}

fn parse_actions_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter.next().map(String::as_str).ok_or_else(|| {
        ParseError::Usage(
            "actions requires `list`, `invoke`, `status`, `cancel` or `session` (try --help)"
                .to_string(),
        )
    })?;
    match verb {
        "session" => match iter.next() {
            None => Ok(Command::ActionsSession),
            Some(extra) => Err(ParseError::Usage(format!(
                "actions session takes no arguments (got `{extra}`); it reads requests on stdin"
            ))),
        },
        "list" => parse_actions_list(iter),
        "invoke" => parse_action_invocation(iter),
        "status" | "cancel" => {
            let raw = iter.next().ok_or_else(|| {
                ParseError::Usage(format!("actions {verb} requires an invocation id"))
            })?;
            let invocation_id = raw.parse::<u64>().map_err(|_| {
                ParseError::Usage(format!("actions {verb}: `{raw}` is not an invocation id"))
            })?;
            let mut wait_ms = None;
            for rest in iter.by_ref() {
                match wait_flag(rest) {
                    Some(wait) if verb == "status" => wait_ms = Some(wait?),
                    _ => {
                        return Err(ParseError::Usage(format!(
                            "actions {verb} accepts exactly one invocation id"
                        )));
                    }
                }
            }
            Ok(if verb == "status" {
                Command::ActionsStatus {
                    invocation_id,
                    wait_ms,
                }
            } else {
                Command::ActionsCancel { invocation_id }
            })
        }
        other => Err(ParseError::Usage(format!(
            "actions: unknown verb `{other}` (expected `list`, `invoke`, `status`, `cancel` or \
             `session`)"
        ))),
    }
}

/// Pull the next argv slot and parse it as `f64`. Helper for the
/// `click-at` / `drag` subcommands so each missing-arg / parse-failure
/// path reads as one line at the call site.
fn next_f64<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
    err_msg: &str,
) -> Result<f64, ParseError> {
    let raw = iter
        .next()
        .ok_or_else(|| ParseError::Usage(err_msg.to_string()))?;
    raw.parse::<f64>()
        .map_err(|e| ParseError::Usage(format!("{err_msg} — `{raw}` is not a number ({e})")))
}

fn next_u32<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
    err_msg: &str,
) -> Result<u32, ParseError> {
    let raw = iter
        .next()
        .ok_or_else(|| ParseError::Usage(err_msg.to_string()))?;
    raw.parse::<u32>()
        .map_err(|e| ParseError::Usage(format!("{err_msg} — `{raw}` is not a count ({e})")))
}

fn optional_u32<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
    err_msg: &str,
) -> Result<Option<u32>, ParseError> {
    let mut rest = iter.peekable();
    if rest.peek().is_none() {
        return Ok(None);
    }
    next_u32(&mut rest, err_msg).map(Some)
}

/// Parse `"ctrl,shift"` into `(ctrl, shift, alt, logo)`. Empty string
/// clears every flag. Unknown flag names error out so a typo doesn't
/// silently drop a modifier on the next chord.
fn parse_modifier_flags(s: &str) -> Result<(bool, bool, bool, bool), ParseError> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut logo = false;
    if s.is_empty() {
        return Ok((false, false, false, false));
    }
    for raw in s.split(',') {
        match raw.trim() {
            "ctrl" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            "logo" => logo = true,
            other => {
                return Err(ParseError::Usage(format!(
                    "modifiers: unknown flag `{other}` (allowed: ctrl,shift,alt,logo)"
                )));
            }
        }
    }
    Ok((ctrl, shift, alt, logo))
}

/// Build the JSON wire payload for a parsed [`Command`]. For `raw`
/// the caller's JSON is used verbatim; every other variant serialises
/// through `serde_json::to_string`.
fn action_arguments(
    text: Option<&str>,
    stdin_pending: bool,
) -> Result<serde_json::Value, ParseError> {
    if stdin_pending {
        return Err(ParseError::Usage(
            "stdin action arguments have not been read".into(),
        ));
    }
    text.map(serde_json::from_str)
        .transpose()
        .map_err(|_| ParseError::Usage("invalid action arguments JSON".into()))
        .map(|value| value.unwrap_or_else(|| serde_json::json!({})))
}

/// `actions list`'s request. The table prints no contracts, so it asks for
/// compact rows; `--json` prints what the desktop answers, so it asks for
/// full ones. `--wait` asks the desktop to hold it until it is complete.
fn actions_list_wire(
    query: &str,
    include_programmatic: bool,
    json: bool,
    page: &ListPage,
    wait_ms: Option<u64>,
) -> Result<serde_json::Value, ParseError> {
    use tilo_control::protocol::{Command, ListingDetail, ListingRequest};
    encode_control(Command::ActionsEnumerate {
        query: query.to_owned(),
        include_programmatic,
        page: ListingRequest {
            limit: page.limit,
            cursor: page.cursor.clone(),
            detail: if json {
                ListingDetail::Full
            } else {
                ListingDetail::Compact
            },
            kinds: page.kinds.clone(),
            files: page.files,
            every_workspace: false,
        },
        wait_ms,
        after_revision: None,
    })
}

fn encode_control(
    command: tilo_control::protocol::Command,
) -> Result<serde_json::Value, ParseError> {
    serde_json::to_value(command).map_err(|error| ParseError::Usage(error.to_string()))
}

fn command_to_wire(cmd: &Command) -> Result<String, ParseError> {
    use serde_json::json;
    let value = match cmd {
        Command::ListTargets => json!({ "op": "list_targets" }),
        Command::Click { name } => json!({ "op": "click_named", "name": name }),
        Command::ClickAt { x, y } => json!({ "op": "click_at", "x": x, "y": y }),
        Command::Drag {
            x1,
            y1,
            x2,
            y2,
            steps,
        } => {
            let mut v = json!({
                "op": "drag",
                "x_from": x1,
                "y_from": y1,
                "x_to": x2,
                "y_to": y2,
            });
            if let Some(s) = steps
                && let Some(obj) = v.as_object_mut()
            {
                obj.insert("steps".to_string(), json!(s));
            }
            v
        }
        Command::Key { key } => json!({ "op": "key", "key": key }),
        Command::Type { text } => json!({ "op": "type", "text": text }),
        Command::Modifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => json!({
            "op": "modifiers",
            "ctrl": ctrl,
            "shift": shift,
            "alt": alt,
            "logo": logo,
        }),
        Command::Snapshot { path } => json!({ "op": "snapshot", "path": path }),
        Command::Swipe {
            fingers,
            dx,
            dy,
            steps,
        } => json!({ "op": "swipe", "fingers": fingers, "dx": dx, "dy": dy, "steps": steps }),
        Command::Pinch {
            fingers,
            scale,
            steps,
        } => json!({ "op": "pinch", "fingers": fingers, "scale": scale, "steps": steps }),
        Command::Watch { watching } => json!({ "op": "watch", "watching": watching }),
        Command::StateDump { history, .. } => {
            encode_control(tilo_control::protocol::Command::StateDump {
                include_history: *history,
            })?
        }
        Command::Windows { .. } => json!({ "op": "windows" }),
        Command::Read { .. } => {
            encode_control(tilo_control::protocol::Command::ActionsEnumerate {
                query: "read".into(),
                include_programmatic: false,
                page: Default::default(),
                wait_ms: None,
                after_revision: None,
            })?
        }
        Command::ActionsList {
            query,
            include_programmatic,
            json,
            page,
            wait_ms,
        } => actions_list_wire(query, *include_programmatic, *json, page, *wait_ms)?,
        Command::ActionsInvoke {
            provider,
            id,
            args_json,
            args_stdin,
            device,
            wait_ms,
        } => invoke_wire(
            provider,
            id,
            &action_arguments(args_json.as_deref(), *args_stdin)?,
            device.as_deref(),
            *wait_ms,
        )
        .map_err(|error| ParseError::Usage(error.to_string()))?,
        Command::ActionsStatus {
            invocation_id,
            wait_ms,
        } => encode_control(tilo_control::protocol::Command::ActionsStatus {
            invocation_id: *invocation_id,
            wait_ms: *wait_ms,
        })?,
        Command::ActionsCancel { invocation_id } => {
            encode_control(tilo_control::protocol::Command::ActionsCancel {
                invocation_id: *invocation_id,
            })?
        }
        // Its requests come from stdin, one per line, and go out as they are.
        Command::ActionsSession => return Ok(String::new()),
        Command::Raw { json } => {
            // Validate here: a `parse_error` from the compositor can't tell
            // the caller whether their shell quoting or the payload was wrong.
            return serde_json::from_str::<serde_json::Value>(json)
                .map(|_| json.clone())
                .map_err(|e| ParseError::Usage(format!("raw: payload is not valid JSON ({e})")));
        }
    };
    Ok(value.to_string())
}

/// Print the compositor's response in the format each subcommand
/// expects. Returns the process exit code (0 on `ok`/`targets`/`state`,
/// 1 on `err`).
/// `Response::Targets(Vec<TargetEntry>)` serialises as `{"targets": [...]}`.
/// Print one per line as `NAME  (X, Y, W, H)`. Split out of
/// [`print_response`] to keep it short.
fn print_list_targets(response: &serde_json::Value) {
    let targets = response
        .get("targets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for entry in targets {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let rect = entry
            .get("rect")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_f64)
                    .map(|f| format!("{f:.0}"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let r = if let [r0, r1, r2, r3] = rect.as_slice() {
            format!("({r0}, {r1}, {r2}, {r3})")
        } else {
            "(?)".to_string()
        };
        println!("{name}  {r}");
    }
}

/// `Response::Answer(v)` serialises as `{"answer": v}`. Split out of
/// [`print_response`] to keep it short.
fn print_actions_list(response: &serde_json::Value, json: bool) -> u8 {
    let answer = response
        .get("answer")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if json {
        println!("{answer}");
        return 0;
    }
    let actions = answer
        .get("actions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if actions.is_empty() {
        // An empty surface is a real answer, not a failure — say
        // so plainly rather than printing nothing and leaving the
        // caller unsure whether the command ran.
        println!("no actions exported for this query");
        return 0;
    }
    for line in action_rows(&actions) {
        println!("{line}");
    }
    if let Some(next) = answer.get("next_cursor").and_then(|v| v.as_str()) {
        let total = answer.get("total").and_then(serde_json::Value::as_u64);
        eprintln!(
            "showing {} of {} actions; the next page: --cursor {next}",
            actions.len(),
            total.map_or_else(|| "more".to_string(), |total| total.to_string()),
        );
    }
    0
}

/// The table rows of `actions list`: `PROVIDER:ID  KIND  TITLE`, aligned.
///
/// Address first: the `(provider, id)` pair is the thing a caller passes
/// back to `actions invoke`, so it leads. Ids are machine keys that join
/// their parts with control characters (`U+001F`); a terminal draws those
/// as nothing, so the parts and the columns ran together. They print as
/// [`printable_id`] escapes, which `actions invoke` takes back.
fn action_rows(actions: &[serde_json::Value]) -> Vec<String> {
    let rows: Vec<(String, &str, &str)> = actions
        .iter()
        .map(|entry| {
            let field = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
            let address = format!(
                "{}:{}",
                printable_id(field("provider").unwrap_or("?")),
                printable_id(field("id").unwrap_or("?"))
            );
            (
                address,
                field("kind").unwrap_or(""),
                field("title").unwrap_or(""),
            )
        })
        .collect();
    let width = rows
        .iter()
        .map(|(address, _, _)| address.chars().count())
        .max()
        .unwrap_or(0);
    let kind_width = rows
        .iter()
        .map(|(_, kind, _)| kind.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(address, kind, title)| {
            format!("{address:<width$}  {kind:<kind_width$}  {title}")
                .trim_end()
                .to_string()
        })
        .collect()
}

fn print_response(cmd: &Command, response: &serde_json::Value) -> u8 {
    if let Some(err) = response.get("err") {
        let code = err
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("error [{code}]: {msg}");
        if let Some(hint) = error_hint(code) {
            eprintln!("hint: {hint}");
        }
        return 1;
    }
    match cmd {
        Command::ListTargets => print_list_targets(response),
        Command::StateDump { pretty, .. } => {
            // `Response::State` serialises as `{"state": {...}}`; print the
            // inner object so consumers can pipe to `jq .workspace_tree`
            // without an outer level.
            let inner = response
                .get("state")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let out = if *pretty {
                serde_json::to_string_pretty(&inner).unwrap_or_else(|_| inner.to_string())
            } else {
                inner.to_string()
            };
            println!("{out}");
        }
        Command::Windows { json } => print_windows(response, *json),
        Command::Snapshot { path } => {
            // The wire's `Ok { frame }` doesn't echo the path back, so print
            // it here to make `snapshot /tmp/x.png | xclip` work.
            println!("{path}");
        }
        Command::ActionsList { json, .. } => return print_actions_list(response, *json),
        Command::ActionsInvoke {
            wait_ms: Some(_), ..
        }
        | Command::ActionsStatus {
            wait_ms: Some(_), ..
        } => return print_settled(response),
        Command::ActionsInvoke { .. }
        | Command::ActionsStatus { .. }
        | Command::ActionsCancel { .. } => {
            let answer = response
                .get("answer")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            println!("{answer}");
        }
        Command::Click { .. }
        | Command::ClickAt { .. }
        | Command::Drag { .. }
        | Command::Swipe { .. }
        | Command::Pinch { .. }
        | Command::Key { .. }
        | Command::Type { .. }
        | Command::Modifiers { .. }
        | Command::Watch { .. } => {
            // Plain Ok response — emit the canonical "ok" string.
            println!("ok");
        }
        Command::Read { .. } | Command::ActionsSession => {
            // Answered by `run_read` and `run_session`, which own their
            // round trips.
            println!("{response}");
        }
        Command::Raw { .. } => {
            // For raw passthrough, dump the response JSON verbatim
            // so the caller can pipe it into `jq` without
            // post-processing.
            println!("{response}");
        }
    }
    0
}

/// Print the `windows` op's rows.
fn print_windows(response: &serde_json::Value, json: bool) {
    let windows = response
        .get("answer")
        .and_then(|answer| answer.get("windows"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if json {
        println!("{}", serde_json::Value::Array(windows));
    } else if windows.is_empty() {
        println!("no windows are open");
    } else {
        for window in &windows {
            let field = |name: &str| {
                window.get(name).map_or_else(String::new, |v| match v {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
            };
            let active = if window.get("active") == Some(&serde_json::Value::Bool(true)) {
                "*"
            } else {
                " "
            };
            println!(
                "{active} {}/{}  group {}  tab {}  {}  {}",
                field("workspace"),
                field("strip"),
                field("group"),
                field("tab"),
                field("app_id"),
                field("title"),
            );
        }
    }
}

/// Print the CLI usage banner to stdout. Mirrors the module-level
/// docstring's subcommand table.
fn print_usage() {
    println!(
        "tilo-control — drive a running Tilo desktop\n\
         \n\
         Usage:\n\
         \x20 tilo-control [--socket PATH] <subcommand> [args]\n\
         \n\
         The action model — the typed actions behind the launcher, the palette, settings\n\
         and every app — is the way to drive a desktop. Run it as the device admin (the\n\
         account that set the device up, over its own login or ssh): the desktop serves\n\
         $XDG_RUNTIME_DIR/tilo-session/admin-control.sock to that account once first login\n\
         is complete. A set $TILO_TEST_CONTROL, or --socket, sends every subcommand there\n\
         instead.\n\
         \n\
         Action model (admin control socket):\n\
         \x20 actions list [QUERY] [--json] [--programmatic] search: list actions as `PROVIDER:ID  KIND  TITLE`\n\
         \x20                               (read-only; empty QUERY lists everything;\n\
         \x20                               --json adds params, contract and hierarchy;\n\
         \x20                               apps' resources and actions come first, files\n\
         \x20                               and folders only with --files or --kind file)\n\
         \x20              [--kind KIND]   only rows of that kind (repeatable): application,\n\
         \x20                               action, note, record, tab, file, …\n\
         \x20              [--limit ROWS] [--cursor NEXT]\n\
         \x20                               one page at a time (default 200 rows); a\n\
         \x20                               longer listing names its next page's cursor\n\
         \x20              [--wait[=MS]]   answer once the listing is complete: no app\n\
         \x20                               index or directory page still on its way\n
         \x20 actions invoke PROVIDER ID [--device DEVICE] [--args-json OBJECT | --args-stdin]\n\
         \x20               [--wait[=MS]] [--no-wait]\n\
         \x20                               run one action by its address; answers once\n\
         \x20                               it has settled (default 5000 ms, at most\n\
         \x20                               30000) with its result, and exits 1 if it\n\
         \x20                               failed. --no-wait answers the admission\n\
         \x20                               receipt. --device sends it to that enrolled\n\
         \x20                               device, which answers once it has it\n\
         \x20 actions status INVOCATION_ID [--wait[=MS]]\n\
         \x20                               latest state and output of a running action;\n\
         \x20                               --wait answers once it has settled\n\
         \x20 actions cancel INVOCATION_ID  request cooperative cancellation\n\
         \x20 actions session               many requests over one connection: one wire\n\
         \x20                               request per stdin line (`raw` JSON), one\n\
         \x20                               answer per stdout line\n\
         \x20 read APP [ITEM] [--json]      what an open app holds, as typed data: the\n\
         \x20                               resource itself, or the ITEM it lists\n\
         \x20                               (e.g. `read so.tilo.notes`, then a page's item)\n\
         \x20 windows [--json]              open windows: workspace/strip, group, tab, app\n\
         \x20                               (`*` marks the one in front)\n\
         \x20 state-dump [--pretty] [--history]\n\
         \x20                               the whole workspace_tree + chrome as JSON;\n\
         \x20                               --history keeps effect receipts and audit\n\
         \x20                               trails, which grow with every action\n\
         \x20 raw '<json>'                  passthrough — escape hatch for new commands\n\
         \n\
         For example: `actions list notes`, then `actions invoke applications so.tilo.notes`,\n\
         then `windows` shows its window. Open a file with `actions invoke files /abs/path`,\n\
         lock with `actions invoke session lock`.\n\
         \n\
         Synthetic input and capture (development sessions only: a dev-overrides build\n\
         started with $TILO_TEST_CONTROL; default $XDG_RUNTIME_DIR/tilo-session/test-control.sock).\n\
         Prefer the action model; these exist for testing pointer, hit-testing and painting:\n\
         \x20 list-targets                  print every target as `NAME  (X, Y, W, H)`\n\
         \x20 click NAME                    click_named\n\
         \x20 click-at X Y                  click_at\n\
         \x20 drag X1 Y1 X2 Y2 [STEPS]      drag\n\
         \x20 swipe FINGERS DX DY [STEPS]   touchpad swipe through the shell's gesture tracker\n\
         \x20 pinch FINGERS SCALE [STEPS]   touchpad pinch to an absolute scale\n\
         \x20 key KEY                       key (named like `Enter`, or single char)\n\
         \x20 type TEXT                     type each character in TEXT\n\
         \x20 modifiers FLAGS               comma-separated subset of ctrl,shift,alt,logo\n\
         \x20                               (empty string clears all)\n\
         \x20 snapshot PATH                 write a PNG of the screen to PATH; answers once\n\
         \x20                               the file is complete\n\
         \x20 watch on|off                  headless: rasterize every frame for a viewer, or stop"
    );
}

/// Read only the opt-in source; malformed input is never quoted in diagnostics.
fn read_action_arguments(
    command: &mut Command,
    reader: impl std::io::Read,
) -> Result<(), ParseError> {
    use std::io::Read as _;
    const LIMIT: u64 = 65_536;
    let Command::ActionsInvoke {
        args_json,
        args_stdin: true,
        ..
    } = command
    else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    reader
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ParseError::Usage("could not read action arguments from stdin".into()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(ParseError::Usage(
            "stdin action arguments exceed 65536 bytes".into(),
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| ParseError::Usage("stdin action arguments must be valid UTF-8".into()))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        ParseError::Usage("stdin action arguments must be a valid JSON object".into())
    })?;
    if !value.is_object() {
        return Err(ParseError::Usage(
            "stdin action arguments must be a JSON object".into(),
        ));
    }
    *args_json = Some(text);
    if let Command::ActionsInvoke { args_stdin, .. } = command {
        *args_stdin = false;
    }
    Ok(())
}

fn main() -> ExitCode {
    // A downstream `head -5` closes its read end before we finish writing,
    // and Rust's default panic-on-EPIPE turns that routine pipe into a
    // process abort. Restoring SIGPIPE's default disposition makes the next
    // failed write exit 141 like every other unix tool.
    //
    // SAFETY: `signal` with `SIG_DFL` is async-signal-safe; we run
    // before any thread is spawned, so no other code observes the
    // change mid-flight.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut invocation = match parse_args(&argv) {
        Ok(inv) => inv,
        Err(ParseError::Help) => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Err(ParseError::Usage(msg)) => {
            eprintln!("tilo-control: {msg}");
            return ExitCode::from(2);
        }
    };

    if let Err(ParseError::Usage(message)) =
        read_action_arguments(&mut invocation.command, std::io::stdin().lock())
    {
        eprintln!("tilo-control: {message}");
        return ExitCode::from(2);
    }

    let cmd_json = match command_to_wire(&invocation.command) {
        Ok(j) => j,
        // `command_to_wire` never produces `Help` — degrade like a usage
        // error rather than assert a state that would need main to panic.
        Err(ParseError::Help) => {
            eprintln!("tilo-control: internal error: unexpected help state");
            return ExitCode::from(2);
        }
        Err(ParseError::Usage(msg)) => {
            eprintln!("tilo-control: {msg}");
            return ExitCode::from(2);
        }
    };

    let (socket, kind) = match resolve_socket(
        invocation.socket.clone(),
        std::env::var_os(TEST_CONTROL_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
        invocation.command.is_semantic(),
    ) {
        Ok(resolved) => resolved,
        Err(ParseError::Usage(msg)) => {
            eprintln!("tilo-control: {msg}");
            return ExitCode::from(2);
        }
        Err(ParseError::Help) => {
            eprintln!("tilo-control: internal error: unexpected help state");
            return ExitCode::from(2);
        }
    };
    let mut stream = match connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tilo-control: connect {}: {e}", socket.display());
            eprintln!("hint: {}", connect_hint(kind, &e));
            return ExitCode::from(2);
        }
    };

    if matches!(invocation.command, Command::ActionsSession) {
        return match run_session(&stream, std::io::stdin().lock(), std::io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tilo-control: session: {e}");
                ExitCode::from(2)
            }
        };
    }

    if let Command::Read { app, item, json } = &invocation.command {
        let code = run_read(&mut stream, app, item.as_deref(), *json);
        return if code == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(code)
        };
    }

    let response = match round_trip(&mut stream, &cmd_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tilo-control: socket round-trip: {e}");
            return ExitCode::from(2);
        }
    };
    if turned_away_by_admin_socket(kind, &response) {
        eprintln!("{TURNED_AWAY}");
        return ExitCode::from(1);
    }
    let code = print_response(&invocation.command, &response);
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code)
    }
}

#[cfg(test)]
mod tests {
    //! Pure-logic argv-parsing tests: every subcommand's parse path, the
    //! resulting `Command` variant, and the wire payload's schema. The
    //! socket protocol itself is covered in
    //! `compositor::test_control::tests` and `tests/control_socket_test.rs`.
    use super::*;

    /// In the Terminal app the admin socket answers `not_permitted`; the
    /// person is pointed at `tilo action`, in plain words.
    #[test]
    fn a_sandboxed_caller_is_pointed_at_tilo_action_without_jargon() {
        let refused = serde_json::json!({"err": {
            "code": "not_permitted",
            "message": "a sandboxed process cannot use the admin control socket"
        }});
        assert!(turned_away_by_admin_socket(SocketKind::Admin, &refused));
        assert!(!turned_away_by_admin_socket(SocketKind::Named, &refused));
        let other = serde_json::json!({"err": {"code": "unknown_action", "message": ""}});
        assert!(!turned_away_by_admin_socket(SocketKind::Admin, &other));
        assert!(TURNED_AWAY.contains("tilo action"));
        for jargon in ["socket", "collaboration", "sandbox", "connection"] {
            assert!(!TURNED_AWAY.contains(jargon), "{jargon} in {TURNED_AWAY}");
        }
    }

    /// Ids join their parts with U+001F, which a terminal draws as nothing:
    /// the table ran the parts and the columns together. Each part shows,
    /// the columns line up, and what is shown invokes the same action.
    #[test]
    fn actions_list_shows_every_part_of_an_id_in_aligned_columns_and_invoke_takes_it_back() {
        let id = "so.tilo.chat\u{1f}read\u{1f}tilo.history\u{1f}\u{1f}history.open";
        let actions = [
            serde_json::json!({
                "provider": "app-actions", "id": id, "kind": "action", "title": "History"
            }),
            serde_json::json!({
                "provider": "settings", "id": "wifi", "kind": "application", "title": "Wi-Fi"
            }),
        ];
        let rows = action_rows(&actions);
        assert!(
            rows.iter().all(|row| !row.chars().any(char::is_control)),
            "{rows:?}"
        );
        let printed = r"app-actions:so.tilo.chat\x1fread\x1ftilo.history\x1f\x1fhistory.open";
        assert!(rows[0].starts_with(printed), "{rows:?}");
        let kind_column = |row: &str, kind: &str| row.find(kind).expect("kind shown");
        assert_eq!(
            kind_column(&rows[0], " action"),
            kind_column(&rows[1], " application"),
            "{rows:?}"
        );
        let title_column = |row: &str, title: &str| row.find(title).expect("title shown");
        assert_eq!(
            title_column(&rows[0], "History"),
            title_column(&rows[1], "Wi-Fi"),
            "{rows:?}"
        );

        let (provider, shown_id) = printed.split_once(':').expect("an address");
        let invoked = parse(&["actions", "invoke", provider, shown_id]).unwrap();
        let Command::ActionsInvoke {
            provider: invoked_provider,
            id: invoked_id,
            ..
        } = invoked.command
        else {
            panic!("an invocation");
        };
        assert_eq!(invoked_provider, "app-actions");
        assert_eq!(invoked_id, id);
        let raw = parse(&["actions", "invoke", "app-actions", id]).unwrap();
        assert!(matches!(raw.command, Command::ActionsInvoke { id: raw_id, .. } if raw_id == id));
    }

    #[test]
    fn programmatic_discovery_is_explicit_and_uses_the_normal_actions_wire() {
        let normal = parse(&["actions", "list", "so.tilo.browser", "--json"]).unwrap();
        assert_eq!(
            wire(&normal.command),
            serde_json::json!({
                "op": "actions_enumerate", "query": "so.tilo.browser"
            })
        );
        let machine = parse(&[
            "actions",
            "list",
            "--programmatic",
            "so.tilo.browser",
            "--json",
        ])
        .unwrap();
        assert_eq!(
            wire(&machine.command),
            serde_json::json!({
                "op": "actions_enumerate", "query": "so.tilo.browser", "include_programmatic": true
            })
        );
        assert!(matches!(
            machine.command,
            Command::ActionsList {
                json: true,
                include_programmatic: true,
                ..
            }
        ));
    }

    /// The table asks for compact rows; paging flags pass through.
    #[test]
    fn actions_list_pages_and_asks_for_compact_rows_for_the_table() {
        let table = parse(&[
            "actions", "list", "open", "--limit", "50", "--cursor", "50.ab",
        ])
        .unwrap();
        assert_eq!(
            wire(&table.command),
            serde_json::json!({
                "op": "actions_enumerate", "query": "open", "detail": "compact",
                "limit": 50, "cursor": "50.ab",
            })
        );
        assert!(parse(&["actions", "list", "--limit", "many"]).is_err());
        assert!(parse(&["actions", "list", "--cursor"]).is_err());
    }

    #[test]
    fn stdin_arguments_use_the_normal_wire_and_redact_local_failures() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic-private-reader-error"))
            }
        }
        let mut invocation = parse(&[
            "actions",
            "invoke",
            "greeter",
            "sign-in@greeter",
            "--args-stdin",
        ])
        .unwrap();
        let supplied = br#"{"password":"synthetic-private-value"}"#;
        read_action_arguments(&mut invocation.command, supplied.as_slice()).unwrap();
        let wire = wire(&invocation.command);
        assert_eq!(wire["op"], "actions_invoke");
        assert_eq!(wire["provider"], "greeter");
        assert_eq!(wire["id"], "sign-in@greeter");
        assert_eq!(wire["args"]["password"], "synthetic-private-value");
        for flags in [
            vec!["--args-stdin", "--args-json", "{}"],
            vec!["--args-json", "{}", "--args-stdin"],
            vec!["--args-stdin", "--args-stdin"],
        ] {
            let mut words = vec!["actions", "invoke", "greeter", "sign-in@greeter"];
            words.extend(flags);
            assert!(parse(&words).is_err());
        }
        for invalid in [
            b"{\"synthetic-private-value\":NaN}".as_slice(),
            b"{\"synthetic-private-value\":\xff}".as_slice(),
            b"[]".as_slice(),
        ] {
            let mut command = parse(&[
                "actions",
                "invoke",
                "greeter",
                "sign-in@greeter",
                "--args-stdin",
            ])
            .unwrap()
            .command;
            let error = read_action_arguments(&mut command, invalid).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic-private-value"));
        }
        let mut command = parse(&[
            "actions",
            "invoke",
            "greeter",
            "sign-in@greeter",
            "--args-stdin",
        ])
        .unwrap()
        .command;
        assert!(
            !format!(
                "{:?}",
                read_action_arguments(&mut command, Broken).unwrap_err()
            )
            .contains("synthetic-private")
        );
        let exact = format!("{{\"value\":\"{}\"}}", "a".repeat(65_524));
        assert_eq!(exact.len(), 65_536);
        read_action_arguments(&mut command, exact.as_bytes()).unwrap();
        let mut command = parse(&[
            "actions",
            "invoke",
            "greeter",
            "sign-in@greeter",
            "--args-stdin",
        ])
        .unwrap()
        .command;
        let too_large = format!("{exact} ");
        assert!(read_action_arguments(&mut command, too_large.as_bytes()).is_err());
        assert!(command_to_wire(&command).is_err());
    }

    fn parse(args: &[&str]) -> Result<Invocation, ParseError> {
        let owned: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
        parse_args(&owned)
    }

    #[test]
    fn touchpad_gestures_parse_with_optional_steps() {
        let cases: Vec<(&[&str], Command)> = vec![
            (
                &["swipe", "3", "-240", "0"],
                Command::Swipe {
                    fingers: 3,
                    dx: -240.0,
                    dy: 0.0,
                    steps: None,
                },
            ),
            (
                &["pinch", "4", "0.5", "6"],
                Command::Pinch {
                    fingers: 4,
                    scale: 0.5,
                    steps: Some(6),
                },
            ),
        ];
        for (args, want) in cases {
            assert_eq!(parse(args).unwrap().command, want, "case {args:?}");
        }
        assert!(
            parse(&["swipe", "3", "-240"]).is_err(),
            "a swipe needs both axes"
        );
        assert!(
            parse(&["pinch", "three", "0.5"]).is_err(),
            "fingers are a count"
        );
    }

    /// Receipt ids parse for `status`, which, like `list`, takes
    /// `--wait[=MS]`; without it neither waits.
    #[test]
    fn waits_parse_on_status_and_list() {
        let status = |words: &[&str]| parse(words).unwrap().command;
        assert_eq!(
            status(&["actions", "status", "42"]),
            Command::ActionsStatus {
                invocation_id: 42,
                wait_ms: None,
            }
        );
        assert_eq!(
            status(&["actions", "status", "42", "--wait=100"]),
            Command::ActionsStatus {
                invocation_id: 42,
                wait_ms: Some(100),
            }
        );
        assert_eq!(
            status(&["actions", "status", "42", "--wait"]),
            Command::ActionsStatus {
                invocation_id: 42,
                wait_ms: Some(DEFAULT_INVOKE_WAIT_MS),
            }
        );
        assert_eq!(
            status(&["actions", "list", "read", "--wait=2000"]),
            Command::ActionsList {
                query: "read".into(),
                json: false,
                include_programmatic: false,
                page: ListPage::default(),
                wait_ms: Some(2000),
            }
        );
        assert!(parse(&["actions", "cancel", "42", "--wait"]).is_err());
        assert!(parse(&["actions", "status", "42", "--wait=soon"]).is_err());

        // `--wait` travels as `wait_ms` on a job read and a listing.
        assert_eq!(
            wire(&Command::ActionsStatus {
                invocation_id: 3,
                wait_ms: Some(10),
            }),
            serde_json::json!({"op": "actions_status", "invocation_id": 3, "wait_ms": 10})
        );
        assert_eq!(
            wire(&Command::ActionsList {
                query: "read".into(),
                json: true,
                include_programmatic: false,
                page: ListPage::default(),
                wait_ms: Some(20),
            }),
            serde_json::json!({"op": "actions_enumerate", "query": "read", "wait_ms": 20})
        );
    }

    #[test]
    fn subcommands_and_modifier_flags_parse_into_their_values() {
        let dump = |pretty| Command::StateDump {
            pretty,
            history: false,
        };
        let drag = |steps| Command::Drag {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
            steps,
        };
        let modifiers = |ctrl, shift| Command::Modifiers {
            ctrl,
            shift,
            alt: false,
            logo: false,
        };
        let cases: Vec<(&[&str], Command)> = vec![
            (&["list-targets"], Command::ListTargets),
            (
                &["click", "btn"],
                Command::Click {
                    name: "btn".to_string(),
                },
            ),
            (
                &["click-at", "100.5", "200"],
                Command::ClickAt { x: 100.5, y: 200.0 },
            ),
            (&["drag", "0", "0", "100", "100"], drag(None)),
            (&["drag", "0", "0", "100", "100", "16"], drag(Some(16))),
            (
                &["key", "Enter"],
                Command::Key {
                    key: "Enter".to_string(),
                },
            ),
            // The OS arg-tokenizer hands us the already-quoted string,
            // so `type "hello world"` arrives as one argv slot.
            (
                &["type", "hello world"],
                Command::Type {
                    text: "hello world".to_string(),
                },
            ),
            (&["modifiers", "ctrl,shift"], modifiers(true, true)),
            (&["modifiers", ""], modifiers(false, false)),
            (
                &["snapshot", "/tmp/x.png"],
                Command::Snapshot {
                    path: "/tmp/x.png".to_string(),
                },
            ),
            (&["watch", "on"], Command::Watch { watching: true }),
            (&["watch", "off"], Command::Watch { watching: false }),
            (&["state-dump"], dump(false)),
            (&["state-dump", "--pretty"], dump(true)),
            (
                &[
                    "actions",
                    "invoke",
                    "app-actions",
                    "peer-address",
                    "--args-json",
                    r#"{"count":2,"force":true}"#,
                ],
                Command::ActionsInvoke {
                    provider: "app-actions".into(),
                    id: "peer-address".into(),
                    args_stdin: false,
                    args_json: Some(r#"{"count":2,"force":true}"#.into()),
                    device: None,
                    wait_ms: Some(DEFAULT_INVOKE_WAIT_MS),
                },
            ),
            // Receipt ids parse for cancel; status in `waits_parse_on_status_and_list`.
            (
                &["actions", "cancel", "42"],
                Command::ActionsCancel { invocation_id: 42 },
            ),
            (
                &["raw", r#"{"op":"list_targets"}"#],
                Command::Raw {
                    json: r#"{"op":"list_targets"}"#.to_string(),
                },
            ),
        ];
        for (args, want) in cases {
            assert_eq!(parse(args).unwrap().command, want, "case {args:?}");
        }

        let inv = parse(&["--socket", "/custom/path", "list-targets"]).unwrap();
        assert_eq!(inv.socket, Some(PathBuf::from("/custom/path")));

        for (flags, want) in [
            ("", (false, false, false, false)),
            ("ctrl,shift,alt,logo", (true, true, true, true)),
            // Tolerate `ctrl, shift` (with a space) — shells often
            // produce that when the user quotes the arg.
            ("ctrl, shift", (true, true, false, false)),
        ] {
            assert_eq!(parse_modifier_flags(flags).unwrap(), want, "case {flags:?}");
        }
    }

    #[test]
    fn malformed_invocations_are_usage_errors_and_help_is_its_own_variant() {
        let usage: [&[&str]; 7] = [
            &["click"],
            &["click-at", "nope", "200"],
            &["modifiers", "meta"],
            &[
                "actions",
                "invoke",
                "app-actions",
                "peer-address",
                "--args-json",
                "[]",
            ],
            &["go-fishing"],
            &["--socket"],
            &["actions", "status", "not-a-number"],
        ];
        for args in usage {
            let err = parse(args).unwrap_err();
            assert!(
                matches!(err, ParseError::Usage(_)),
                "case {args:?}: {err:?}"
            );
        }
        for flag in ["--help", "-h"] {
            assert_eq!(parse(&[flag]).unwrap_err(), ParseError::Help, "case {flag}");
        }
    }

    #[test]
    fn the_action_model_and_input_route_to_their_own_sockets() {
        for semantic in [
            &["actions", "list"][..],
            &["actions", "invoke", "applications", "so.tilo.notes"],
            &["actions", "status", "7"],
            &["state-dump"],
            &["windows"],
            &["windows", "--json"],
        ] {
            assert!(
                parse(semantic).unwrap().command.is_semantic(),
                "{semantic:?}"
            );
        }
        for input in [
            &["click-at", "1", "2"][..],
            &["type", "x"],
            &["snapshot", "shot.png"],
            &["list-targets"],
        ] {
            assert!(!parse(input).unwrap().command.is_semantic(), "{input:?}");
        }
        assert!(parse(&["windows", "--pretty"]).is_err());
    }

    #[test]
    fn read_finds_the_apps_read_action_and_parses_its_item() {
        assert_eq!(
            parse(&["read", "so.tilo.notes", "notes.page:1", "--json"])
                .unwrap()
                .command,
            Command::Read {
                app: "so.tilo.notes".into(),
                item: Some("notes.page:1".into()),
                json: true,
            }
        );
        assert!(parse(&["read"]).is_err(), "an app id is required");
        assert!(parse(&["read", "a", "b", "c"]).is_err());
        assert!(
            parse(&["read", "so.tilo.notes"])
                .unwrap()
                .command
                .is_semantic()
        );

        let answer = serde_json::json!({"actions": [
            {"provider": "applications", "id": "so.tilo.notes", "application": null, "operation": null},
            {"provider": "app-actions", "id": "other", "application": "so.tilo.files", "operation": "read"},
            {"provider": "app-actions", "id": "x", "application": "so.tilo.notes", "operation": "render"},
            {"provider": "app-actions", "id": "wanted", "application": "so.tilo.notes", "operation": "read"},
        ]});
        assert_eq!(
            read_action_id(&answer, "so.tilo.notes").as_deref(),
            Some("wanted")
        );
        assert_eq!(read_action_id(&answer, "so.tilo.chat"), None);

        let succeeded = serde_json::json!({"answer": {"receipt": {"update": {
            "state": "succeeded", "output": {"value": {"pages": []}}}}}});
        assert_eq!(print_read_answer(&succeeded, true), 0);
        let failed = serde_json::json!({"answer": {"receipt": {"update": {
            "state": "failed", "error": {"code": "action_failed", "message": "no note"}}}}});
        assert_eq!(print_read_answer(&failed, true), 1);
    }

    /// A scripted desktop: answers `replies` in order and records each
    /// request it was sent.
    fn scripted(
        replies: Vec<serde_json::Value>,
        sent: &mut Vec<serde_json::Value>,
    ) -> impl FnMut(&str) -> std::io::Result<serde_json::Value> + '_ {
        let mut replies = replies.into_iter();
        move |request| {
            sent.push(serde_json::from_str(request).expect("requests are JSON"));
            replies
                .next()
                .ok_or_else(|| std::io::Error::other("the script has no more answers"))
        }
    }

    fn read_listing(revision: &str, with_read: bool) -> serde_json::Value {
        let actions = if with_read {
            serde_json::json!([{
                "provider": "app-actions", "id": "notes-read",
                "application": "so.tilo.notes", "operation": "read",
            }])
        } else {
            serde_json::json!([])
        };
        serde_json::json!({"answer": {
            "actions": actions, "revision": revision, "complete": true, "settled": true,
        }})
    }

    /// `read` asks the desktop to wait on both the listing and the app's
    /// answer, so it takes one listing and one invocation: no pausing, no
    /// polling.
    #[test]
    fn read_waits_in_two_requests_and_never_polls() {
        let answered = serde_json::json!({"answer": {"receipt": {
            "broker_invocation_id": 7,
            "update": {"state": "succeeded", "output": {"value": {"pages": []}}}}}});
        let mut sent = Vec::new();
        let code = run_read_with(
            &mut scripted(vec![read_listing("r1", true), answered], &mut sent),
            "so.tilo.notes",
            Some("page-1"),
            true,
        );
        assert_eq!(code, 0);
        assert_eq!(sent.len(), 2, "one listing, one invocation: {sent:?}");
        assert_eq!(sent[0]["op"], "actions_enumerate");
        assert_eq!(sent[0]["wait_ms"], READ_DISCOVERY_WAIT_MS);
        assert!(sent[0].get("after_revision").is_none());
        assert_eq!(
            sent[1],
            serde_json::json!({
                "op": "actions_invoke", "provider": "app-actions", "id": "notes-read",
                "args": {"item": "page-1"}, "wait_ms": READ_ANSWER_WAIT_MS,
            })
        );

        // An app still starting is not listed yet: the next listing is asked
        // for past the one seen, so the desktop holds it until it changes.
        let mut sent = Vec::new();
        let code = run_read_with(
            &mut scripted(
                vec![
                    read_listing("r1", false),
                    read_listing("r2", true),
                    serde_json::json!({"answer": {"receipt": {
                        "update": {"state": "succeeded", "output": {"value": 1}}}}}),
                ],
                &mut sent,
            ),
            "so.tilo.notes",
            None,
            true,
        );
        assert_eq!(code, 0);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[1]["after_revision"], "r1");

        // A desktop that does not wait (no `settled`) is asked once: asking
        // again would only spin.
        let mut sent = Vec::new();
        let code = run_read_with(
            &mut scripted(
                vec![serde_json::json!({"answer": {"actions": [], "revision": "r1"}})],
                &mut sent,
            ),
            "so.tilo.notes",
            None,
            true,
        );
        assert_eq!(code, 1);
        assert_eq!(sent.len(), 1);
    }

    /// `read` waits inside the desktop: the client holds no pause of its
    /// own to tune.
    #[test]
    fn read_contains_no_sleep() {
        let source = include_str!("main.rs");
        let read = source
            .split("fn run_read_with(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("run_read_with is defined here");
        assert!(!read.contains("sleep"), "run_read_with must not sleep");
    }

    /// An app's read the desktop's wait did not see answered prints no
    /// receipt as if it were content.
    #[test]
    fn read_never_prints_an_unanswered_receipt_as_content() {
        let admitted = serde_json::json!({"answer": {"receipt": {
            "broker_invocation_id": 7, "update": {"state": "accepted"}}}});
        assert_eq!(print_read_answer(&admitted, true), 1);
        let answered = serde_json::json!({"answer": {"receipt": {
            "broker_invocation_id": 7,
            "update": {"state": "succeeded", "output": {"value": {"pages": []}}}}}});
        assert_eq!(print_read_answer(&answered, true), 0);
    }

    /// `--wait` asks the desktop to hold the answer until the action
    /// settles; without it the answer stays the admission receipt.
    #[test]
    fn invoke_wait_asks_the_desktop_for_the_settled_outcome() {
        let invoke = |extra: &[&str]| {
            let mut words = vec!["actions", "invoke", "app-actions", "notes.new"];
            words.extend_from_slice(extra);
            parse(&words).map(|invocation| invocation.command)
        };
        // The result is the answer: an invocation waits unless told not to.
        let plain = invoke(&[]).unwrap();
        assert!(matches!(
            plain,
            Command::ActionsInvoke {
                wait_ms: Some(DEFAULT_INVOKE_WAIT_MS),
                ..
            }
        ));
        assert_eq!(wire(&plain)["wait_ms"], DEFAULT_INVOKE_WAIT_MS);
        let admitted = invoke(&["--no-wait"]).unwrap();
        assert!(matches!(
            admitted,
            Command::ActionsInvoke { wait_ms: None, .. }
        ));
        assert_eq!(wire(&admitted).get("wait_ms"), None);
        assert!(matches!(
            invoke(&["--device", "aa"]).unwrap(),
            Command::ActionsInvoke { wait_ms: None, .. }
        ));
        let waited = invoke(&["--wait"]).unwrap();
        assert!(matches!(
            waited,
            Command::ActionsInvoke {
                wait_ms: Some(DEFAULT_INVOKE_WAIT_MS),
                ..
            }
        ));
        assert_eq!(wire(&waited)["wait_ms"], DEFAULT_INVOKE_WAIT_MS);
        assert_eq!(wire(&invoke(&["--wait=250"]).unwrap())["wait_ms"], 250);
        assert!(matches!(
            invoke(&["--wait=soon"]),
            Err(ParseError::Usage(_))
        ));
        assert!(matches!(
            invoke(&["--wait", "--device", "aa"]),
            Err(ParseError::Usage(_))
        ));
    }

    #[test]
    fn a_waited_invocation_exits_by_how_it_ended() {
        let answer = |update: serde_json::Value| {
            serde_json::json!({"answer": {"receipt": {
                "broker_invocation_id": 7, "update": update}}})
        };
        let settled = answer(serde_json::json!({"state": "succeeded", "output": {}}));
        assert_eq!(print_settled(&settled), 0);
        let failed = answer(serde_json::json!({"state": "failed",
            "error": {"code": "action_failed", "message": "no note"}}));
        assert_eq!(print_settled(&failed), 1);
        assert_eq!(
            print_settled(&answer(serde_json::json!({"state": "cancelled"}))),
            1
        );
        let running = answer(serde_json::json!({"state": "progress", "phase": "copying"}));
        assert_eq!(print_settled(&running), 0, "still running is no failure");
    }

    #[test]
    fn a_session_answers_each_request_line_in_order_over_one_connection() {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let desktop = std::thread::spawn(move || {
            let mut replies = &server;
            let mut requests = BufReader::new(&server);
            let mut seen = Vec::new();
            let mut line = String::new();
            while requests.read_line(&mut line).expect("request") > 0 {
                let request: serde_json::Value =
                    serde_json::from_str(line.trim()).expect("one JSON request per line");
                seen.push(request["op"].as_str().unwrap_or_default().to_string());
                writeln!(
                    replies,
                    "{}",
                    serde_json::json!({"answer": {"op": request["op"]}})
                )
                .expect("reply");
                line.clear();
            }
            seen
        });
        let requests =
            b"{\"op\":\"actions_enumerate\"}\n\n{\"op\":\"actions_status\",\"invocation_id\":1}\n";
        let mut answers = Vec::new();
        run_session(&client, &requests[..], &mut answers).expect("session");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("end of requests");
        assert_eq!(
            desktop.join().expect("desktop"),
            ["actions_enumerate", "actions_status"],
            "one request per non-blank line, in order"
        );
        let answers = String::from_utf8(answers).expect("utf-8 answers");
        assert_eq!(
            answers.lines().collect::<Vec<_>>(),
            [
                r#"{"answer":{"op":"actions_enumerate"}}"#,
                r#"{"answer":{"op":"actions_status"}}"#,
            ]
        );
        assert!(matches!(
            parse(&["actions", "session"]).map(|invocation| invocation.command),
            Ok(Command::ActionsSession)
        ));
        assert!(matches!(
            parse(&["actions", "session", "extra"]),
            Err(ParseError::Usage(_))
        ));
    }

    /// History is opt-in, beside `--pretty` and in any order.
    #[test]
    fn state_dump_takes_history_beside_pretty() {
        assert_eq!(
            parse(&["state-dump", "--history", "--pretty"])
                .unwrap()
                .command,
            Command::StateDump {
                pretty: true,
                history: true,
            }
        );
        assert!(parse(&["state-dump", "--json"]).is_err());
    }

    #[test]
    fn socket_resolution_prefers_flag_then_env_then_runtime_dir() {
        let flag = Some(PathBuf::from("/flag.sock"));
        let env = Some(OsString::from("/env.sock"));
        let runtime = Some(OsString::from("/run/user/1000"));
        for semantic in [true, false] {
            assert_eq!(
                resolve_socket(flag.clone(), env.clone(), runtime.clone(), semantic).unwrap(),
                (PathBuf::from("/flag.sock"), SocketKind::Named)
            );
            assert_eq!(
                resolve_socket(None, env.clone(), runtime.clone(), semantic).unwrap(),
                (PathBuf::from("/env.sock"), SocketKind::Named)
            );
        }
        // The action model needs no environment: it goes to the admin socket.
        assert_eq!(
            resolve_socket(None, None, runtime.clone(), true).unwrap(),
            (
                PathBuf::from("/run/user/1000/tilo-session/admin-control.sock"),
                SocketKind::Admin
            )
        );
        assert_eq!(
            resolve_socket(None, None, runtime, false).unwrap(),
            (
                PathBuf::from("/run/user/1000/tilo-session/test-control.sock"),
                SocketKind::TestControl
            )
        );

        // No usable runtime dir means no default rather than /tmp.
        for runtime in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            // An empty TILO_TEST_CONTROL counts as unset, not as a path.
            let err = resolve_socket(None, Some(OsString::new()), runtime, true).unwrap_err();
            let ParseError::Usage(message) = err else {
                panic!("expected a usage error, got {err:?}");
            };
            assert!(message.contains("--socket"), "{message}");
            assert!(message.contains("TILO_TEST_CONTROL"), "{message}");
        }
    }

    // ===================================================================
    // Wire payload encoding — guards the JSON shape per subcommand.
    // ===================================================================

    fn wire(command: &Command) -> serde_json::Value {
        serde_json::from_str(&command_to_wire(command).unwrap()).unwrap()
    }

    #[test]
    fn wire_payloads_carry_exactly_each_commands_fields() {
        assert_eq!(wire(&Command::ListTargets)["op"], "list_targets");

        let v = wire(&Command::Click {
            name: "btn".to_string(),
        });
        assert_eq!(v["op"], "click_named");
        assert_eq!(v["name"], "btn");

        let v = wire(&Command::ClickAt { x: 1.5, y: 2.5 });
        assert_eq!(v["op"], "click_at");
        assert_eq!(v["x"], 1.5);
        assert_eq!(v["y"], 2.5);

        let drag = |steps| Command::Drag {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
            steps,
        };
        let v = wire(&drag(None));
        assert_eq!(v["op"], "drag");
        assert!(v.get("steps").is_none(), "steps must be absent when None");
        assert_eq!(wire(&drag(Some(4)))["steps"], 4);

        let v = wire(&Command::Modifiers {
            ctrl: true,
            shift: false,
            alt: true,
            logo: false,
        });
        assert_eq!(v["op"], "modifiers");
        assert_eq!(v["ctrl"], true);
        assert_eq!(v["shift"], false);
        assert_eq!(v["alt"], true);
        assert_eq!(v["logo"], false);

        let v = wire(&Command::StateDump {
            pretty: true,
            history: false,
        });
        // `pretty` is purely a print-time concern; the wire payload
        // never carries it (the compositor doesn't need to know). Without
        // `--history` the request is the one every compositor understands.
        assert_eq!(v, serde_json::json!({ "op": "state_dump" }));
        assert_eq!(
            wire(&Command::StateDump {
                pretty: false,
                history: true,
            }),
            serde_json::json!({ "op": "state_dump", "include_history": true })
        );
        assert_eq!(
            wire(&Command::Windows { json: true }),
            serde_json::json!({ "op": "windows" })
        );

        let value = wire(&Command::ActionsInvoke {
            provider: "app-actions".into(),
            id: "7-resource-action".into(),
            args_stdin: false,
            args_json: Some(r#"{"resource":"notes.note:42","at":"2026-09-01T12:00:00Z"}"#.into()),
            device: None,
            wait_ms: None,
        });
        assert_eq!(value["op"], "actions_invoke");
        assert_eq!(value["args"]["resource"], "notes.note:42");
        assert_eq!(value["args"]["at"], "2026-09-01T12:00:00Z");

        for (command, operation) in [
            (
                Command::ActionsStatus {
                    invocation_id: 91,
                    wait_ms: None,
                },
                "actions_status",
            ),
            (
                Command::ActionsCancel { invocation_id: 91 },
                "actions_cancel",
            ),
        ] {
            let value = wire(&command);
            assert_eq!(value["op"], operation, "case {operation}");
            assert_eq!(value["invocation_id"], 91, "case {operation}");
        }

        let json = command_to_wire(&Command::Raw {
            json: r#"{"op":"list_targets","extra":1}"#.to_string(),
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"list_targets","extra":1}"#);
        let err = command_to_wire(&Command::Raw {
            json: "not json".to_string(),
        })
        .unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }
}

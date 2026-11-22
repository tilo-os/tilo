//! `tilo-control` — drive a running Tilo desktop through its control sockets.
//!
//! Wraps the compositor's JSON control protocol (one request and one response
//! per line, defined by `shell::shell::test_control`) so scripts don't have to
//! glue `socat` to `jq`. Two sockets speak it:
//!
//! - the device admin's control socket,
//!   `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`, which every build
//!   serves in the device admin's session once first login is complete. It
//!   answers the action model — its frames, `state-dump`, `windows` — and is
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
use std::path::PathBuf;
use std::process::ExitCode;

use tilo_control::client::{connect, round_trip};
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
             model instead: `tilo action list`, `tilo do`, `windows`."
        ),
        (SocketKind::Named, _) => {
            "is the desktop running, and is this the socket it bound?".to_string()
        }
    }
}

/// What a person reads when the admin's socket turns their process away:
/// the desktop's own words for why (another account, a sandboxed app, a
/// Terminal tab it no longer shows, or a request it does not serve), which
/// stay accurate as the socket's rules change.
fn admin_socket_refusal(kind: SocketKind, response: &serde_json::Value) -> Option<String> {
    if kind != SocketKind::Admin {
        return None;
    }
    tilo_control::client::refusal_of_process(
        &tilo_control::client::ControlSocket::Admin(PathBuf::new()),
        response,
    )
    .map(|message| format!("tilo-control: {message}"))
}

/// A next step for a refusal the compositor answered with.
fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "session_locked" => Some("unlock the session at the device, then retry"),
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
        /// The button held through the drag (`left`, `right` or `middle`); the left one when
        /// `None`.
        button: Option<String>,
    },
    /// A mouse wheel turned at `(x, y)`, `dy` notches up (positive) and `dx` across, with the
    /// modifiers held while it turns: Ctrl and a wheel zoom a canvas.
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        ctrl: bool,
        shift: bool,
        alt: bool,
        logo: bool,
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
    /// Unlock the session from here, as an accepted password does.
    Unlock,
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
    Raw {
        json: String,
    },
    /// The questions waiting for the person, as their sheets show them: each
    /// one's handle, kind and label, and the action that answers or declines
    /// it (a development session's socket only).
    Questions {
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
            Self::StateDump { .. } | Self::Windows { .. } | Self::Raw { .. }
        )
    }
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
        "drag" => parse_drag(&mut iter)?,
        "wheel" => parse_wheel(&mut iter)?,
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
        "unlock" => Command::Unlock,
        "watch" => parse_watch(iter.next())?,
        "state-dump" | "windows" => parse_state_read(op, &mut iter)?,
        "questions" => {
            let json = match iter.next().map(String::as_str) {
                None => false,
                Some("--json") => true,
                Some(other) => {
                    return Err(ParseError::Usage(format!(
                        "questions takes no arguments or --json (got `{other}`)"
                    )));
                }
            };
            Command::Questions { json }
        }
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

/// Parse `drag X1 Y1 X2 Y2 [STEPS] [--button left|right|middle]`.
fn parse_drag<'a>(iter: &mut impl Iterator<Item = &'a String>) -> Result<Command, ParseError> {
    let x1 = next_f64(iter, "drag X1 Y1 X2 Y2 [STEPS] [--button B]")?;
    let y1 = next_f64(iter, "drag X1 Y1 X2 Y2 [STEPS] [--button B]")?;
    let x2 = next_f64(iter, "drag X1 Y1 X2 Y2 [STEPS] [--button B]")?;
    let y2 = next_f64(iter, "drag X1 Y1 X2 Y2 [STEPS] [--button B]")?;
    let mut steps = None;
    let mut button = None;
    while let Some(arg) = iter.next() {
        if arg == "--button" {
            let name = iter.next().ok_or_else(|| {
                ParseError::Usage("drag --button takes left, right or middle".to_string())
            })?;
            if !matches!(name.as_str(), "left" | "right" | "middle") {
                return Err(ParseError::Usage(format!(
                    "drag --button is left, right or middle, not `{name}`"
                )));
            }
            button = Some(name.clone());
        } else {
            steps = Some(
                arg.parse::<u32>()
                    .map_err(|e| ParseError::Usage(format!("drag STEPS must be u32 ({e})")))?,
            );
        }
    }
    Ok(Command::Drag {
        x1,
        y1,
        x2,
        y2,
        steps,
        button,
    })
}

/// Parse `wheel X Y DY [DX] [--with FLAGS]`.
fn parse_wheel<'a>(iter: &mut impl Iterator<Item = &'a String>) -> Result<Command, ParseError> {
    const USAGE: &str = "wheel X Y DY [DX] [--with FLAGS]";
    let x = next_f64(iter, USAGE)?;
    let y = next_f64(iter, USAGE)?;
    let dy = next_f64(iter, USAGE)?;
    let mut dx = 0.0;
    let mut flags = (false, false, false, false);
    while let Some(arg) = iter.next() {
        if arg == "--with" {
            let given = iter.next().ok_or_else(|| {
                ParseError::Usage(
                    "wheel --with takes a comma-separated subset of ctrl,shift,alt,logo"
                        .to_string(),
                )
            })?;
            flags = parse_modifier_flags(given)?;
        } else {
            dx = arg
                .parse::<f64>()
                .map_err(|e| ParseError::Usage(format!("wheel DX must be a number ({e})")))?;
        }
    }
    let (ctrl, shift, alt, logo) = flags;
    Ok(Command::Wheel {
        x,
        y,
        dx,
        dy,
        ctrl,
        shift,
        alt,
        logo,
    })
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

/// Parse `swipe` and `pinch`. Split out of [`parse_args`] to keep it short.
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

/// The wire payload of a pointer command the other subcommands do not share: a drag with its
/// button, a wheel with its modifiers.
fn pointer_to_wire(cmd: &Command) -> serde_json::Value {
    use serde_json::json;
    match cmd {
        Command::Drag {
            x1,
            y1,
            x2,
            y2,
            steps,
            button,
        } => {
            let mut v = json!({
                "op": "drag",
                "x_from": x1,
                "y_from": y1,
                "x_to": x2,
                "y_to": y2,
            });
            if let Some(obj) = v.as_object_mut() {
                if let Some(s) = steps {
                    obj.insert("steps".to_string(), json!(s));
                }
                if let Some(button) = button {
                    obj.insert("button".to_string(), json!(button));
                }
            }
            v
        }
        Command::Wheel {
            x,
            y,
            dx,
            dy,
            ctrl,
            shift,
            alt,
            logo,
        } => json!({
            "op": "scroll",
            "unit": "notch",
            "x": x,
            "y": y,
            "dx": dx,
            "dy": dy,
            "ctrl": ctrl,
            "shift": shift,
            "alt": alt,
            "logo": logo,
        }),
        _ => serde_json::Value::Null,
    }
}

/// Build the JSON wire payload for a parsed [`Command`]. For `raw`
/// the caller's JSON is used verbatim; every other variant serialises
/// through `serde_json::to_string`.
fn command_to_wire(cmd: &Command) -> Result<String, ParseError> {
    use serde_json::json;
    let value = match cmd {
        Command::ListTargets => json!({ "op": "list_targets" }),
        Command::Click { name } => json!({ "op": "click_named", "name": name }),
        Command::ClickAt { x, y } => json!({ "op": "click_at", "x": x, "y": y }),
        Command::Drag { .. } | Command::Wheel { .. } => pointer_to_wire(cmd),
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
        Command::Unlock => json!({ "op": "unlock" }),
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
        Command::StateDump { history: true, .. } => {
            json!({ "op": "state_dump", "include_history": true })
        }
        Command::StateDump { .. } => json!({ "op": "state_dump" }),
        Command::Windows { .. } => json!({ "op": "windows" }),
        Command::Questions { .. } => json!({ "op": "questions" }),
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
        Command::Unlock => {
            let was_locked =
                response.pointer("/ok/was_locked") == Some(&serde_json::Value::Bool(true));
            println!("{}", if was_locked { "unlocked" } else { "not locked" });
        }
        Command::Click { .. }
        | Command::ClickAt { .. }
        | Command::Drag { .. }
        | Command::Wheel { .. }
        | Command::Swipe { .. }
        | Command::Pinch { .. }
        | Command::Key { .. }
        | Command::Type { .. }
        | Command::Modifiers { .. }
        | Command::Watch { .. } => {
            // Plain Ok response — emit the canonical "ok" string.
            println!("ok");
        }
        Command::Questions { json } => print_questions(response, *json),
        Command::Raw { .. } => {
            // For raw passthrough, dump the response JSON verbatim
            // so the caller can pipe it into `jq` without
            // post-processing.
            println!("{response}");
        }
    }
    0
}

/// Print the `questions` op's rows: the sheets waiting for the person, and
/// the `tilo do` call that answers or declines each.
fn print_questions(response: &serde_json::Value, json: bool) {
    let questions = response
        .get("answer")
        .and_then(|answer| answer.get("questions"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if json {
        println!("{}", serde_json::Value::Array(questions));
    } else if questions.is_empty() {
        println!("no question is waiting");
    } else {
        for question in &questions {
            let field = |name: &str| {
                question
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let resource = field("resource");
            println!(
                "{} — {} ({})",
                field("label"),
                field("kind"),
                field("handle")
            );
            for (verb, action) in [("answer", "answer"), ("decline", "cancel")] {
                println!("  {verb}:  tilo do {resource}#{action}");
            }
        }
    }
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
            // Where the frame draws the window, which is what a tool aiming
            // at it aims at: `x,y wxh`, and nothing for a tab no window fills.
            let rect = window
                .get("rect")
                .and_then(serde_json::Value::as_array)
                .map(|rect| {
                    let n = |at: usize| {
                        rect.get(at)
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or_default()
                    };
                    format!("  at {},{} {}x{}", n(0), n(1), n(2), n(3))
                })
                .unwrap_or_default();
            println!(
                "{active} {}/{}  group {}  tab {}  {}  {}{rect}",
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
         and every app — is the way to drive a desktop: `tilo action list` and `tilo do`\n\
         send its frames to the desktop's control socket. The desktop serves\n\
         $XDG_RUNTIME_DIR/tilo-session/admin-control.sock to the device admin (the account\n\
         that set the device up, over its own login or ssh) once first login is complete.\n\
         A set $TILO_TEST_CONTROL, or --socket, sends every subcommand there instead.\n\
         \n\
         \x20 windows [--json]              open windows: workspace/strip, group, tab, app\n\
         \x20                               (`*` marks the one in front)\n\
         \x20 state-dump [--pretty] [--history]\n\
         \x20                               the whole workspace_tree + chrome as JSON;\n\
         \x20                               --history keeps effect receipts and audit\n\
         \x20                               trails, which grow with every action\n\
         \x20 raw '<json>' | raw -          one request line as it is (`-`: read from stdin):\n\
         \x20                               an action frame for a console app (the\n\
         \x20                               installer, the greeter)\n\
         \n\
         For example: `tilo open notes`, then `windows` shows its window.\n\
         \n\
         Synthetic input and capture (development sessions only: a dev-overrides build\n\
         started with $TILO_TEST_CONTROL; default $XDG_RUNTIME_DIR/tilo-session/test-control.sock).\n\
         Prefer the action model; these exist for testing pointer, hit-testing and painting:\n\
         \x20 list-targets                  print every target as `NAME  (X, Y, W, H)`\n\
         \x20 click NAME                    click_named\n\
         \x20 click-at X Y                  click_at\n\
         \x20 drag X1 Y1 X2 Y2 [STEPS] [--button left|right|middle]\n\
         \x20                               drag with that button held (left by default)\n\
         \x20 wheel X Y DY [DX] [--with FLAGS]\n\
         \x20                               turn the wheel at X Y, DY notches up, holding\n\
         \x20                               FLAGS (ctrl,shift,alt,logo) while it turns\n\
         \x20 swipe FINGERS DX DY [STEPS]   touchpad swipe through the shell's gesture tracker\n\
         \x20 pinch FINGERS SCALE [STEPS]   touchpad pinch to an absolute scale\n\
         \x20 key KEY                       key (named like `Enter`, or single char)\n\
         \x20 type TEXT                     type each character in TEXT\n\
         \x20 modifiers FLAGS               comma-separated subset of ctrl,shift,alt,logo\n\
         \x20                               (empty string clears all)\n\
         \x20 snapshot PATH                 write a PNG of the screen to PATH; answers once\n\
         \x20                               the file is complete, and is refused while the\n\
         \x20                               session is locked\n\
         \x20 unlock                        unlock the session, as an accepted password does\n\
         \x20 questions [--json]            the questions waiting for the person, each with\n\
         \x20                               the `tilo do` call that answers or declines it: a check driving a sheet\n\
         \x20                               (a device asking to be unlocked) answers it here\n\
         \x20 watch on|off                  headless: rasterize every frame for a viewer, or stop"
    );
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
    // `raw -` reads its line from stdin, so a frame carrying a secret never reaches argv.
    if let Command::Raw { json } = &mut invocation.command
        && json == "-"
    {
        use std::io::Read as _;
        json.clear();
        if std::io::stdin().lock().read_to_string(json).is_err() {
            eprintln!("tilo-control: raw -: could not read the request from stdin");
            return ExitCode::from(2);
        }
        json.truncate(json.trim_end().len());
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

    let response = match round_trip(&mut stream, &cmd_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tilo-control: socket round-trip: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(refusal) = admin_socket_refusal(kind, &response) {
        eprintln!("{refusal}");
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
    //! `shell::test_control::tests`.
    use super::*;

    /// The admin socket's refusal is shown in its own words, so the reason
    /// a person reads is the one the desktop applied.
    #[test]
    fn an_admin_socket_refusal_is_shown_in_the_desktops_words() {
        let refused = serde_json::json!({"err": {
            "code": "not_permitted",
            "message": "programs inside a sandboxed app can't use tilo-control"
        }});
        assert_eq!(
            admin_socket_refusal(SocketKind::Admin, &refused).as_deref(),
            Some("tilo-control: programs inside a sandboxed app can't use tilo-control")
        );
        assert_eq!(admin_socket_refusal(SocketKind::Named, &refused), None);
        let other = serde_json::json!({"err": {"code": "unknown_action", "message": ""}});
        assert_eq!(admin_socket_refusal(SocketKind::Admin, &other), None);
    }

    /// Ids join their parts with U+001F, which a terminal draws as nothing:
    /// the table ran the parts and the columns together. Each part shows,
    /// the columns line up, and what is shown invokes the same action.
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

    /// The canvas's camera from a script: a wheel at a point with the modifiers held while it
    /// turns (Ctrl and a wheel zoom), and a drag with the middle button held (it pans).
    #[test]
    fn a_wheel_with_modifiers_and_a_middle_drag_parse_and_go_on_the_wire() {
        let parse = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            parse_args(&argv).unwrap().command
        };
        let wheel = parse(&["wheel", "640", "400", "2", "--with", "ctrl"]);
        assert_eq!(
            wheel,
            Command::Wheel {
                x: 640.0,
                y: 400.0,
                dx: 0.0,
                dy: 2.0,
                ctrl: true,
                shift: false,
                alt: false,
                logo: false,
            }
        );
        let v: serde_json::Value = serde_json::from_str(&command_to_wire(&wheel).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "op": "scroll", "unit": "notch", "x": 640.0, "y": 400.0, "dx": 0.0, "dy": 2.0,
                "ctrl": true, "shift": false, "alt": false, "logo": false,
            })
        );
        assert!(matches!(
            parse(&["wheel", "1", "2", "-1", "3"]),
            Command::Wheel { dx, dy, ctrl: false, .. }
                if (dx - 3.0).abs() < f64::EPSILON && (dy + 1.0).abs() < f64::EPSILON
        ));
        let drag = parse(&["drag", "0", "0", "10", "20", "6", "--button", "middle"]);
        let v: serde_json::Value = serde_json::from_str(&command_to_wire(&drag).unwrap()).unwrap();
        assert_eq!(v["button"], "middle");
        assert_eq!(v["steps"], 6);
        let argv: Vec<String> = ["drag", "0", "0", "1", "1", "--button", "thumb"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert!(
            parse_args(&argv).is_err(),
            "a button the pointer has not is refused"
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
            button: None,
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
        let usage: [&[&str]; 6] = [
            &["click"],
            &["click-at", "nope", "200"],
            &["modifiers", "meta"],
            &["go-fishing"],
            &["--socket"],
            &["actions", "list"],
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
        for semantic in [&["state-dump"][..], &["windows"], &["windows", "--json"]] {
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
            button: None,
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

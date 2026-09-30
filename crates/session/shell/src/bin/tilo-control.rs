//! `tilo-control` — thin CLI over the `TILO_TEST_CONTROL` socket.
//!
//! Wraps the JSON command protocol of [`shell::compositor::test_control`] so
//! scripts don't have to glue `socat` to `jq`. The socket path comes from
//! `$TILO_TEST_CONTROL` — the same env var the compositor binds against —
//! falling back to `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`. No
//! `clap`, no async, just stdlib + `serde_json`. Run `--help` for the
//! subcommand list.
//!
//! Exit codes: `0` on `Ok`/`Targets`/`State`, `1` on `Err` (echoing
//! `err.code`/`err.message` to stderr), `2` on argv / I/O / parse failures.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use shell::shell::test_control_socket::{TEST_CONTROL_ENV, default_socket_path};

/// Pick the socket: `--socket`, else `$TILO_TEST_CONTROL`, else the default
/// in the private runtime directory. Never a shared directory: without a
/// runtime directory the caller has to name the socket.
fn resolve_socket(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    runtime_dir: Option<OsString>,
) -> Result<PathBuf, ParseError> {
    flag.or_else(|| env.filter(|env| !env.is_empty()).map(PathBuf::from))
        .or_else(|| default_socket_path(runtime_dir))
        .ok_or_else(|| {
            ParseError::Usage(format!(
                "no control socket: pass --socket PATH or set ${TEST_CONTROL_ENV} \
                 (XDG_RUNTIME_DIR is unset or relative, so there is no default)"
            ))
        })
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
    },
    /// List the actions the shell exports right now. Read-only.
    ActionsList {
        query: String,
        json: bool,
    },
    /// Invoke one enumerated action by its stable `(provider, id)`
    /// address — no synthetic input, no window, no scene.
    ActionsInvoke {
        provider: String,
        id: String,
        args_json: Option<String>,
    },
    ActionsStatus {
        invocation_id: u64,
    },
    ActionsCancel {
        invocation_id: u64,
    },
    Raw {
        json: String,
    },
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
        "state-dump" => {
            let mut pretty = false;
            for rest in iter.by_ref() {
                if rest == "--pretty" {
                    pretty = true;
                } else {
                    return Err(ParseError::Usage(format!(
                        "state-dump: unknown flag `{rest}` (only --pretty)"
                    )));
                }
            }
            Command::StateDump { pretty }
        }
        "actions" => parse_actions_command(&mut iter)?,
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

fn parse_actions_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter.next().map(String::as_str).ok_or_else(|| {
        ParseError::Usage(
            "actions requires `list`, `invoke`, `status`, or `cancel` (try --help)".to_string(),
        )
    })?;
    match verb {
        "list" => {
            // Everything after `list` that isn't a flag joins the
            // query, so `actions list open settings` works without
            // the caller having to quote it.
            let mut json = false;
            let mut words: Vec<String> = Vec::new();
            for rest in iter.by_ref() {
                if rest == "--json" {
                    json = true;
                } else {
                    words.push(rest.clone());
                }
            }
            Ok(Command::ActionsList {
                query: words.join(" "),
                json,
            })
        }
        "invoke" => {
            let provider = iter.next().ok_or_else(|| {
                ParseError::Usage("actions invoke PROVIDER ID requires a provider".to_string())
            })?;
            let id = iter.next().ok_or_else(|| {
                ParseError::Usage("actions invoke PROVIDER ID requires an id".to_string())
            })?;
            let mut args_json = None;
            while let Some(flag) = iter.next() {
                match flag.as_str() {
                    "--args-json" => {
                        let json = iter.next().ok_or_else(|| {
                            ParseError::Usage("--args-json requires a JSON object".to_string())
                        })?;
                        let parsed: serde_json::Value =
                            serde_json::from_str(json).map_err(|error| {
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
                        return Err(ParseError::Usage(format!(
                            "actions invoke: unknown option `{other}`"
                        )));
                    }
                }
            }
            Ok(Command::ActionsInvoke {
                provider: provider.clone(),
                id: id.clone(),
                args_json,
            })
        }
        "status" | "cancel" => {
            let raw = iter.next().ok_or_else(|| {
                ParseError::Usage(format!("actions {verb} requires an invocation id"))
            })?;
            let invocation_id = raw.parse::<u64>().map_err(|_| {
                ParseError::Usage(format!("actions {verb}: `{raw}` is not an invocation id"))
            })?;
            if iter.next().is_some() {
                return Err(ParseError::Usage(format!(
                    "actions {verb} accepts exactly one invocation id"
                )));
            }
            Ok(if verb == "status" {
                Command::ActionsStatus { invocation_id }
            } else {
                Command::ActionsCancel { invocation_id }
            })
        }
        other => Err(ParseError::Usage(format!(
            "actions: unknown verb `{other}` (expected `list`, `invoke`, `status`, or `cancel`)"
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
        Command::StateDump { .. } => json!({ "op": "state_dump" }),
        Command::ActionsList { query, .. } => {
            json!({ "op": "actions_enumerate", "query": query })
        }
        Command::ActionsInvoke {
            provider,
            id,
            args_json,
        } => {
            let args = args_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|error| ParseError::Usage(format!("invalid action arguments: {error}")))?
                .unwrap_or_else(|| serde_json::json!({}));
            json!({ "op": "actions_invoke", "provider": provider, "id": id, "args": args })
        }
        Command::ActionsStatus { invocation_id } => {
            json!({ "op": "actions_status", "invocation_id": invocation_id })
        }
        Command::ActionsCancel { invocation_id } => {
            json!({ "op": "actions_cancel", "invocation_id": invocation_id })
        }
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

/// Connect to the unix socket at `path`. Returns a usage-style error
/// message wrapped in a string so the caller can format it however
/// it likes — the binary's `main` prints this and exits 2.
fn connect(path: &PathBuf) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Send `cmd_json` to the compositor and read one response line.
/// Both directions are line-delimited; the function pushes `\n` and
/// reads up to the first newline.
fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> std::io::Result<serde_json::Value> {
    let mut line = cmd_json.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut response = String::new();
    reader.read_line(&mut response)?;
    serde_json::from_str(response.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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
    // Address first: the `(provider, id)` pair is the thing a
    // caller passes back to `actions invoke`, so it leads.
    let width = actions
        .iter()
        .filter_map(|a| {
            let p = a.get("provider").and_then(|v| v.as_str())?;
            let i = a.get("id").and_then(|v| v.as_str())?;
            Some(p.len().saturating_add(i.len()).saturating_add(1))
        })
        .max()
        .unwrap_or(0);
    for entry in &actions {
        let provider = entry
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let address = format!("{provider}:{id}");
        println!("{address:<width$}  {kind:<12}  {title}");
    }
    0
}

fn print_response(cmd: &Command, response: &serde_json::Value) -> u8 {
    if let Some(err) = response.get("err") {
        let code = err
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("error [{code}]: {msg}");
        return 1;
    }
    match cmd {
        Command::ListTargets => print_list_targets(response),
        Command::StateDump { pretty } => {
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
        Command::Snapshot { path } => {
            // The wire's `Ok { frame }` doesn't echo the path back, so print
            // it here to make `snapshot /tmp/x.png | xclip` work.
            println!("{path}");
        }
        Command::ActionsList { json, .. } => return print_actions_list(response, *json),
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
        Command::Raw { .. } => {
            // For raw passthrough, dump the response JSON verbatim
            // so the caller can pipe it into `jq` without
            // post-processing.
            println!("{response}");
        }
    }
    0
}

/// Print the CLI usage banner to stdout. Mirrors the module-level
/// docstring's subcommand table.
fn print_usage() {
    println!(
        "tilo-control — drive a running tilo-desktop's test_control socket\n\
         \n\
         Usage:\n\
         \x20 tilo-control [--socket PATH] <subcommand> [args]\n\
         \n\
         Default socket path comes from $TILO_TEST_CONTROL, falling back to\n\
         $XDG_RUNTIME_DIR/tilo-session/test-control.sock.\n\
         \n\
         Subcommands:\n\
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
         \x20 snapshot PATH                 snapshot — write PNG to PATH\n\
         \x20 watch on|off                  headless: rasterize every frame for a viewer, or stop\n\
         \x20 state-dump [--pretty]         dump workspace_tree + chrome as JSON\n\
         \x20 actions list [QUERY] [--json] list exported actions as `PROVIDER:ID  KIND  TITLE`\n\
         \x20                               (read-only; empty QUERY lists everything)\n\
         \x20 actions invoke PROVIDER ID [--args-json OBJECT]\n\
         \x20                               run one typed action without synthetic input\n\
         \x20 actions status INVOCATION_ID   read latest broker state/output\n\
         \x20 actions cancel INVOCATION_ID   request cooperative cancellation\n\
         \x20 raw '<json>'                  passthrough — escape hatch for new commands"
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
    let invocation = match parse_args(&argv) {
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

    let socket = match resolve_socket(
        invocation.socket.clone(),
        std::env::var_os(TEST_CONTROL_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        Ok(socket) => socket,
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

    #[test]
    fn subcommands_and_modifier_flags_parse_into_their_values() {
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
            // Drag takes four floats with optional steps.
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
            // The empty string clears every modifier.
            (&["modifiers", ""], modifiers(false, false)),
            (
                &["snapshot", "/tmp/x.png"],
                Command::Snapshot {
                    path: "/tmp/x.png".to_string(),
                },
            ),
            (&["watch", "on"], Command::Watch { watching: true }),
            (&["watch", "off"], Command::Watch { watching: false }),
            (&["state-dump"], Command::StateDump { pretty: false }),
            (
                &["state-dump", "--pretty"],
                Command::StateDump { pretty: true },
            ),
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
                    args_json: Some(r#"{"count":2,"force":true}"#.into()),
                },
            ),
            // Receipt ids parse for both status and cancel.
            (
                &["actions", "status", "42"],
                Command::ActionsStatus { invocation_id: 42 },
            ),
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

        // The socket flag overrides the default.
        let inv = parse(&["--socket", "/custom/path", "list-targets"]).unwrap();
        assert_eq!(inv.socket, Some(PathBuf::from("/custom/path")));

        // The modifier-flag parser itself.
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
    fn socket_resolution_prefers_flag_then_env_then_runtime_dir() {
        let flag = Some(PathBuf::from("/flag.sock"));
        let env = Some(OsString::from("/env.sock"));
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            resolve_socket(flag, env.clone(), runtime.clone()).unwrap(),
            PathBuf::from("/flag.sock")
        );
        assert_eq!(
            resolve_socket(None, env, runtime.clone()).unwrap(),
            PathBuf::from("/env.sock")
        );
        assert_eq!(
            resolve_socket(None, None, runtime).unwrap(),
            PathBuf::from("/run/user/1000/tilo-session/test-control.sock")
        );

        // No usable runtime dir means no default rather than /tmp.
        for runtime in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            // An empty TILO_TEST_CONTROL counts as unset, not as a path.
            let err = resolve_socket(None, Some(OsString::new()), runtime).unwrap_err();
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

        // Drag includes steps when set and omits them when unset.
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

        let v = wire(&Command::StateDump { pretty: true });
        assert_eq!(v["op"], "state_dump");
        // `pretty` is purely a print-time concern; the wire payload
        // never carries it (the compositor doesn't need to know).
        assert!(v.get("pretty").is_none());

        // Action invocations preserve structured arguments.
        let value = wire(&Command::ActionsInvoke {
            provider: "app-actions".into(),
            id: "7-resource-action".into(),
            args_json: Some(r#"{"resource":"notes.note:42","at":"2026-09-01T12:00:00Z"}"#.into()),
        });
        assert_eq!(value["op"], "actions_invoke");
        assert_eq!(value["args"]["resource"], "notes.note:42");
        assert_eq!(value["args"]["at"], "2026-09-01T12:00:00Z");

        // Status and cancel preserve the broker id.
        for (command, operation) in [
            (
                Command::ActionsStatus { invocation_id: 91 },
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

        // Raw passes through verbatim, and invalid raw JSON is a usage error.
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

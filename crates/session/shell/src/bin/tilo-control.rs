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

use serde_json::json;

use shell::shell::search::providers::actions::PROVIDER_ID as SHELL_ACTIONS;
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
    /// Show what the recorder holds and what waits for review.
    FeedbackStatus,
    /// Show the open replay, its transport and its steps.
    ReplayStatus,
    /// A `community …` or `public-room …` operation, already in wire form.
    Community {
        wire: serde_json::Value,
    },
    /// List the app connections that answer inspection.
    InspectApps,
    /// Read one app's semantic tree through the broker.
    Inspect {
        target: InspectTarget,
        query: serde_json::Value,
        local: bool,
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
        "feedback" => parse_feedback_command(&mut iter)?,
        "replay" => parse_replay_command(&mut iter)?,
        "community" => parse_community_command(&mut iter)?,
        "public-room" => parse_public_room_command(&mut iter)?,
        "inspect" => parse_inspect_command(&mut iter)?,
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

/// Parse `community join|leave|status|team-room|direct-room PERSON [LABEL]`.
fn parse_community_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter.next().map(String::as_str).ok_or_else(|| {
        ParseError::Usage(
            "community requires `join`, `leave`, `status`, `team-room` or `direct-room` (try --help)"
                .to_string(),
        )
    })?;
    let wire = match verb {
        "join" => json!({ "op": "community_join" }),
        "leave" => json!({ "op": "community_leave" }),
        "status" => json!({ "op": "community_status" }),
        "team-room" => json!({ "op": "community_team_room" }),
        "direct-room" => {
            let person = iter.next().ok_or_else(|| {
                ParseError::Usage("community direct-room PERSON requires a person id".to_string())
            })?;
            let label: Vec<String> = iter.cloned().collect();
            let mut wire = json!({ "op": "community_direct_room", "person": person });
            if !label.is_empty()
                && let Some(object) = wire.as_object_mut()
            {
                object.insert("label".to_string(), json!(label.join(" ")));
            }
            wire
        }
        other => {
            return Err(ParseError::Usage(format!(
                "community: unknown verb `{other}` (join, leave, status, team-room, direct-room)"
            )));
        }
    };
    Ok(Command::Community { wire })
}

/// Parse `public-room create NAME | invite ROOM [--role R] [--expires-in S] |
/// status ROOM | grant ROOM PERSON [--role R] | revoke ROOM INVITE_ID`.
fn parse_public_room_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter.next().map(String::as_str).ok_or_else(|| {
        ParseError::Usage(
            "public-room requires `create`, `invite`, `status`, `grant` or `revoke` (try --help)"
                .to_string(),
        )
    })?;
    let wire = match verb {
        "create" => {
            let name: Vec<String> = iter.cloned().collect();
            if name.is_empty() {
                return Err(ParseError::Usage(
                    "public-room create NAME requires a name".to_string(),
                ));
            }
            json!({ "op": "public_room_create", "name": name.join(" ") })
        }
        "invite" => {
            let room = iter.next().ok_or_else(|| {
                ParseError::Usage("public-room invite ROOM requires a room id".to_string())
            })?;
            let mut wire = json!({ "op": "standing_invite", "room": room });
            while let Some(flag) = iter.next() {
                let value = iter.next().ok_or_else(|| {
                    ParseError::Usage(format!("public-room invite: `{flag}` needs a value"))
                })?;
                let (key, value) = match flag.as_str() {
                    "--role" => ("role", json!(value)),
                    "--expires-in" => (
                        "expires_in_seconds",
                        json!(value.parse::<u64>().map_err(|_| {
                            ParseError::Usage("--expires-in takes seconds".to_string())
                        })?),
                    ),
                    other => {
                        return Err(ParseError::Usage(format!(
                            "public-room invite: unknown flag `{other}` (--role, --expires-in)"
                        )));
                    }
                };
                if let Some(object) = wire.as_object_mut() {
                    object.insert(key.to_string(), value);
                }
            }
            wire
        }
        "status" => {
            let room = iter.next().ok_or_else(|| {
                ParseError::Usage("public-room status ROOM requires a room id".to_string())
            })?;
            json!({ "op": "room_status", "room": room })
        }
        "grant" => {
            let (Some(room), Some(person)) = (iter.next(), iter.next()) else {
                return Err(ParseError::Usage(
                    "public-room grant ROOM PERSON [--role viewer|commenter|editor]".to_string(),
                ));
            };
            let role = match (iter.next().map(String::as_str), iter.next()) {
                (None, _) => "editor".to_string(),
                (Some("--role"), Some(role)) => role.clone(),
                _ => {
                    return Err(ParseError::Usage(
                        "public-room grant ROOM PERSON [--role viewer|commenter|editor]"
                            .to_string(),
                    ));
                }
            };
            json!({ "op": "room_grant", "room": room, "person": person, "role": role })
        }
        "revoke" => {
            let (Some(room), Some(invite_id), None) = (iter.next(), iter.next(), iter.next())
            else {
                return Err(ParseError::Usage(
                    "public-room revoke ROOM INVITE_ID".to_string(),
                ));
            };
            json!({ "op": "standing_invite_revoke", "room": room, "invite_id": invite_id })
        }
        other => {
            return Err(ParseError::Usage(format!(
                "public-room: unknown verb `{other}` (create, invite, status, grant, revoke)"
            )));
        }
    };
    Ok(Command::Community { wire })
}

/// Parse the `actions list|invoke ...` subcommand. Split out of
/// [`parse_args`] to keep it short.
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

const FEEDBACK_USAGE: &str = "feedback record start [--room ROOM]... [--app APP]... [--pixels check|keep|off] | \
feedback record stop | feedback record capture-recent | \
feedback recent-buffer on|off | feedback status";

fn feedback_invoke(id: &str, args: &serde_json::Value) -> Command {
    Command::ActionsInvoke {
        provider: SHELL_ACTIONS.to_string(),
        id: id.to_string(),
        args_json: Some(args.to_string()),
    }
}

/// Parse `feedback ...` into the shell actions it names. Every verb is the
/// same shell action Ctrl+K and agents invoke; this only spells
/// the arguments.
fn parse_feedback_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter
        .next()
        .ok_or_else(|| ParseError::Usage(format!("feedback requires a verb: {FEEDBACK_USAGE}")))?;
    match verb.as_str() {
        "status" => Ok(Command::FeedbackStatus),
        "recent-buffer" => match iter.next().map(String::as_str) {
            Some("on") => Ok(feedback_invoke(
                "feedback.recent-buffer.set",
                &serde_json::json!({"enabled": true}),
            )),
            Some("off") => Ok(feedback_invoke(
                "feedback.recent-buffer.set",
                &serde_json::json!({"enabled": false}),
            )),
            _ => Err(ParseError::Usage(
                "feedback recent-buffer requires `on` or `off`".to_string(),
            )),
        },
        "record" => match iter.next().map(String::as_str) {
            Some("start") => parse_feedback_start(iter),
            Some("stop") => Ok(feedback_invoke(
                "feedback.record.stop",
                &serde_json::json!({}),
            )),
            Some("capture-recent") => Ok(feedback_invoke(
                "feedback.record.capture-recent",
                &serde_json::json!({}),
            )),
            _ => Err(ParseError::Usage(
                "feedback record requires `start`, `stop`, or `capture-recent`".to_string(),
            )),
        },
        other => Err(ParseError::Usage(format!(
            "feedback: unknown verb `{other}` (usage: {FEEDBACK_USAGE})"
        ))),
    }
}

const REPLAY_USAGE: &str = "replay open RECORD [--offscreen] | replay play | replay pause | \
replay next | replay previous | replay seek STEP | replay speed FACTOR | replay close | \
replay status";

/// Parse `replay ...` into the shell actions it names: the same
/// `shell.actions` operations Ctrl+K and agents invoke.
fn parse_replay_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter
        .next()
        .ok_or_else(|| ParseError::Usage(format!("replay requires a verb: {REPLAY_USAGE}")))?;
    let value = |iter: &mut dyn Iterator<Item = &'a String>, what: &str| {
        iter.next()
            .cloned()
            .ok_or_else(|| ParseError::Usage(format!("replay {verb} requires {what}")))
    };
    Ok(match verb.as_str() {
        "status" => Command::ReplayStatus,
        "open" => {
            let record = value(iter, "a session record id")?;
            let offscreen = match iter.next().map(String::as_str) {
                None => false,
                Some("--offscreen") => true,
                Some(other) => {
                    return Err(ParseError::Usage(format!(
                        "replay open: unknown option `{other}` (--offscreen)"
                    )));
                }
            };
            feedback_invoke(
                "replay.open",
                &serde_json::json!({"record": record, "offscreen": offscreen}),
            )
        }
        "play" => feedback_invoke("replay.play", &serde_json::json!({})),
        "pause" => feedback_invoke("replay.pause", &serde_json::json!({})),
        "next" => feedback_invoke("replay.step.next", &serde_json::json!({})),
        "previous" => feedback_invoke("replay.step.previous", &serde_json::json!({})),
        "close" => feedback_invoke("replay.close", &serde_json::json!({})),
        "seek" => {
            let step: u64 = value(iter, "a step index")?
                .parse()
                .map_err(|_| ParseError::Usage("replay seek: STEP is a step index".into()))?;
            feedback_invoke("replay.seek", &serde_json::json!({ "step": step }))
        }
        "speed" => {
            let speed: f64 = value(iter, "a factor")?
                .parse()
                .map_err(|_| ParseError::Usage("replay speed: FACTOR is a number".into()))?;
            feedback_invoke("replay.speed", &serde_json::json!({ "speed": speed }))
        }
        other => {
            return Err(ParseError::Usage(format!(
                "replay: unknown verb `{other}` (usage: {REPLAY_USAGE})"
            )));
        }
    })
}

fn parse_feedback_start<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let mut rooms: Vec<String> = Vec::new();
    let mut apps: Vec<String> = Vec::new();
    let mut pixels: Vec<String> = Vec::new();
    while let Some(flag) = iter.next() {
        let target = match flag.as_str() {
            "--room" => &mut rooms,
            "--app" => &mut apps,
            "--pixels" => &mut pixels,
            other => {
                return Err(ParseError::Usage(format!(
                    "feedback record start: unknown option `{other}` (--room ROOM, --app APP, --pixels check|keep|off)"
                )));
            }
        };
        let value = iter
            .next()
            .ok_or_else(|| ParseError::Usage(format!("{flag} requires a value")))?;
        target.push(value.clone());
    }
    let mut args = serde_json::Map::new();
    if !rooms.is_empty() {
        args.insert("rooms".into(), rooms.join(",").into());
    }
    if !apps.is_empty() {
        args.insert("apps".into(), apps.join(",").into());
    }
    if let Some(purpose) = pixels.pop() {
        args.insert("pixels".into(), purpose.into());
    }
    Ok(feedback_invoke(
        "feedback.record.start",
        &serde_json::Value::Object(args),
    ))
}

/// The app an `inspect` names: a broker connection, or one of its windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectTarget {
    Peer(u64),
    Surface(u64),
}

/// How long the CLI waits for an app to answer, polling the compositor
/// (which itself never waits).
const INSPECT_WAIT: std::time::Duration = std::time::Duration::from_secs(6);
const INSPECT_POLL: std::time::Duration = std::time::Duration::from_millis(40);

const INSPECT_USAGE: &str = "inspect requires `apps`, `at PEER X Y`, `node PEER ID` or \
     `tree PEER [--root ID] [--depth N]` (try --help)";

/// Parse the `inspect apps|at|node|tree ...` subcommand.
fn parse_inspect_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter
        .next()
        .map(String::as_str)
        .ok_or_else(|| ParseError::Usage(INSPECT_USAGE.to_string()))?;
    if verb == "apps" {
        if iter.next().is_some() {
            return Err(ParseError::Usage("inspect apps takes no arguments".into()));
        }
        return Ok(Command::InspectApps);
    }
    let peer_raw = iter.next().ok_or_else(|| {
        ParseError::Usage(format!(
            "inspect {verb} requires the PEER id of an app connection (list them with `inspect apps`)"
        ))
    })?;
    let target = match peer_raw.strip_prefix("surface:") {
        Some(surface) => surface.parse::<u64>().map(InspectTarget::Surface),
        None => peer_raw.parse::<u64>().map(InspectTarget::Peer),
    }
    .map_err(|_| {
        ParseError::Usage(format!(
            "inspect {verb}: `{peer_raw}` is not a PEER id or surface:ID (list peers with `inspect apps`)"
        ))
    })?;
    let mut positional: Vec<&String> = Vec::new();
    let mut local = false;
    let mut root: Option<String> = None;
    let mut depth: u16 = 2;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--local" => local = true,
            "--root" if verb == "tree" => {
                root = Some(
                    iter.next()
                        .ok_or_else(|| ParseError::Usage("--root requires a semantic id".into()))?
                        .clone(),
                );
            }
            "--depth" if verb == "tree" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| ParseError::Usage("--depth requires a number".into()))?;
                depth = raw
                    .parse()
                    .map_err(|_| ParseError::Usage(format!("--depth: `{raw}` is not a depth")))?;
            }
            flag if flag.starts_with("--") => {
                return Err(ParseError::Usage(format!(
                    "inspect {verb}: unknown option `{flag}`"
                )));
            }
            _ => positional.push(arg),
        }
    }
    let query = match (verb, positional.as_slice()) {
        ("at", [x, y]) => {
            let coordinate = |raw: &str| {
                raw.parse::<f64>().map_err(|_| {
                    ParseError::Usage(format!("inspect at: `{raw}` is not a coordinate"))
                })
            };
            serde_json::json!({"kind": "at", "x": coordinate(x)?, "y": coordinate(y)?})
        }
        ("at", _) => {
            return Err(ParseError::Usage(
                "inspect at PEER X Y takes a point in that app's surface coordinates".into(),
            ));
        }
        ("node", [id]) => serde_json::json!({"kind": "node", "id": id}),
        ("node", _) => {
            return Err(ParseError::Usage(
                "inspect node PEER ID takes exactly one semantic id".into(),
            ));
        }
        ("tree", []) => serde_json::json!({"kind": "subtree", "root": root, "depth": depth}),
        ("tree", _) => {
            return Err(ParseError::Usage(
                "inspect tree PEER takes only --root ID and --depth N".into(),
            ));
        }
        _ => return Err(ParseError::Usage(INSPECT_USAGE.to_string())),
    };
    Ok(Command::Inspect {
        target,
        query,
        local,
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
        Command::StateDump { .. } | Command::FeedbackStatus | Command::ReplayStatus => {
            json!({ "op": "state_dump" })
        }
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
        Command::Community { wire } => wire.clone(),
        Command::InspectApps => json!({ "op": "inspect_apps" }),
        Command::Inspect {
            target,
            query,
            local,
        } => {
            let (peer, surface) = match target {
                InspectTarget::Peer(peer) => (Some(*peer), None),
                InspectTarget::Surface(surface) => (None, Some(*surface)),
            };
            json!({ "op": "inspect", "peer": peer, "surface": surface, "query": query, "local": local })
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

/// Poll the compositor for the answer to the inspection `started` names,
/// returning the final response envelope.
fn await_inspection(
    socket: &PathBuf,
    started: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(inspection) = started
        .get("answer")
        .and_then(|answer| answer.get("inspection"))
        .and_then(serde_json::Value::as_u64)
    else {
        // A refusal: print it as it came.
        return Ok(started);
    };
    let deadline = std::time::Instant::now()
        .checked_add(INSPECT_WAIT)
        .ok_or("the inspection deadline does not fit the clock")?;
    let request = json!({ "op": "inspect_answer", "inspection": inspection }).to_string();
    loop {
        let mut stream = connect(socket).map_err(|error| format!("connect: {error}"))?;
        let response =
            round_trip(&mut stream, &request).map_err(|error| format!("round-trip: {error}"))?;
        let pending = response
            .get("answer")
            .and_then(|answer| answer.get("pending"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !pending {
            return Ok(response);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the application did not answer inspection {inspection} within {} s",
                INSPECT_WAIT.as_secs()
            ));
        }
        std::thread::sleep(INSPECT_POLL);
    }
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
        Command::FeedbackStatus | Command::ReplayStatus => {
            let part = if matches!(cmd, Command::ReplayStatus) {
                "replay"
            } else {
                "feedback"
            };
            let feedback = response
                .get("state")
                .and_then(|state| state.get("chrome"))
                .and_then(|chrome| chrome.get(part))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&feedback).unwrap_or_else(|_| feedback.to_string())
            );
        }
        Command::Snapshot { path } => {
            // The wire's `Ok { frame }` doesn't echo the path back, so print
            // it here to make `snapshot /tmp/x.png | xclip` work.
            println!("{path}");
        }
        Command::ActionsList { json, .. } => return print_actions_list(response, *json),
        Command::InspectApps | Command::Inspect { .. } => {
            let answer = response
                .get("answer")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&answer).unwrap_or_else(|_| answer.to_string())
            );
        }
        Command::ActionsInvoke { .. }
        | Command::ActionsStatus { .. }
        | Command::ActionsCancel { .. }
        | Command::Community { .. } => {
            let answer = response
                .get("answer")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            println!("{answer}");
        }
        Command::Click { .. }
        | Command::ClickAt { .. }
        | Command::Drag { .. }
        | Command::Key { .. }
        | Command::Type { .. }
        | Command::Modifiers { .. } => {
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
         \x20 key KEY                       key (named like `Enter`, or single char)\n\
         \x20 type TEXT                     type each character in TEXT\n\
         \x20 modifiers FLAGS               comma-separated subset of ctrl,shift,alt,logo\n\
         \x20                               (empty string clears all)\n\
         \x20 snapshot PATH                 snapshot — write PNG to PATH\n\
         \x20 state-dump [--pretty]         dump workspace_tree + chrome as JSON\n\
         \x20 actions list [QUERY] [--json] list exported actions as `PROVIDER:ID  KIND  TITLE`\n\
         \x20                               (read-only; empty QUERY lists everything)\n\
         \x20 actions invoke PROVIDER ID [--args-json OBJECT]\n\
         \x20                               run one typed action without synthetic input\n\
         \x20 actions status INVOCATION_ID   read latest broker state/output\n\
         \x20 actions cancel INVOCATION_ID   request cooperative cancellation\n\
         \x20 feedback record start [--room ROOM]... [--app APP]... [--pixels check|keep|off]\n\
         \x20                               admit the Recorder agent (Observer) to ROOM (default:\n\
         \x20                               the active workspace) and record its documents,\n\
         \x20                               presence and actions; --app narrows the documents.\n\
         \x20                               Masked frames of those apps' windows are kept on\n\
         \x20                               this device to check replay against (--pixels\n\
         \x20                               check, the default); keep: with the recording;\n\
         \x20                               off: no pixels.\n\
         \x20                               The Recorder is listed as AGENT while it records.\n\
         \x20 feedback record stop          remove the Recorder and keep the `session` record\n\
         \x20                               in memory for review; nothing is written or sent\n\
         \x20 feedback record capture-recent\n\
         \x20                               build a session from the last two minutes\n\
         \x20 feedback recent-buffer on|off keep (or forget) those two minutes; off by default\n\
         \x20 feedback status               rooms held, the recording, sessions awaiting review\n\
         \x20                               (the `chrome.feedback` part of state-dump)\n\
         \x20 replay open RECORD [--offscreen]\n\
         \x20                               follow a recorded session in a room of its own;\n\
         \x20                               the room is never saved, synced or offered\n\
         \x20 replay play | pause | next | previous | seek STEP | speed FACTOR | close\n\
         \x20                               transport; steps are attempts and view changes\n\
         \x20 replay status                 transport and step list (`chrome.replay`)\n\
         \x20 community join                join the Tilo community room from the OS's\n\
         \x20                               standing invitation; prints its class and your role\n\
         \x20 community leave               leave the Tilo community room; none of your devices\n\
         \x20                               joins it again until you `community join`\n\
         \x20 community status              the community room as this session knows it\n\
         \x20 community team-room           open your private feedback room with the Tilo team\n\
         \x20 community direct-room PERSON [LABEL]\n\
         \x20                               open the direct room with PERSON (the team's end)\n\
         \x20 public-room create NAME       create a public (signed, unencrypted) room here\n\
         \x20 public-room invite ROOM [--role viewer|commenter] [--expires-in SECONDS]\n\
         \x20                               mint its standing invitation (multi-use, max commenter)\n\
         \x20 public-room status ROOM       a room's channel class and your role in it\n\
         \x20 public-room grant ROOM PERSON [--role viewer|commenter|editor]\n\
         \x20                               grant a role in a room you govern (default editor)\n\
         \x20 public-room revoke ROOM INVITE_ID\n\
         \x20                               stop redeeming a standing invitation here; its\n\
         \x20                               members stay (revoke it at the room's hosts too)\n\
         \x20 inspect apps                 list app connections that answer inspection\n\
         \x20                               (PEER id, app id, room, record)\n\
         \x20                               PEER below may also be surface:ID, a window's surface\n\
         \x20                               id, to inspect the app behind that window\n\
         \x20 inspect at PEER X Y [--local] the semantic node under X,Y (that app's surface\n\
         \x20                               coordinates, logical px) and its path from the root\n\
         \x20 inspect node PEER ID [--local]\n\
         \x20                               one node by stable semantic id (r:…, t:…, …~k:…,\n\
         \x20                               or a Parent[i]/Child[j] path)\n\
         \x20 inspect tree PEER [--root ID] [--depth N] [--local]\n\
         \x20                               a subtree, breadth first (default: window root, depth 2)\n\
         \x20                               Each node: id, durable, role, label, value, sensitivity,\n\
         \x20                               bounds, visible, children, reference (ResourceRef).\n\
         \x20                               Secrets are never shown. Personal content (names,\n\
         \x20                               addresses, message bodies) is a same-length ▮ placeholder\n\
         \x20                               unless --local asks for it as this session sees it.\n\
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

    // The compositor answers an inspection at once with its id; the app's
    // answer is collected here, so the compositor never waits on an app.
    let response = if matches!(invocation.command, Command::Inspect { .. }) {
        match await_inspection(&socket, response) {
            Ok(response) => response,
            Err(message) => {
                eprintln!("tilo-control: {message}");
                return ExitCode::from(1);
            }
        }
    } else {
        response
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
    fn replay_verbs_are_the_shell_replay_actions() {
        let invoke = |id: &str, args: serde_json::Value| Command::ActionsInvoke {
            provider: SHELL_ACTIONS.into(),
            id: id.into(),
            args_json: Some(args.to_string()),
        };
        assert_eq!(
            parse(&["replay", "open", "uuid:1", "--offscreen"])
                .unwrap()
                .command,
            invoke(
                "replay.open",
                serde_json::json!({"record": "uuid:1", "offscreen": true})
            )
        );
        assert_eq!(
            parse(&["replay", "seek", "3"]).unwrap().command,
            invoke("replay.seek", serde_json::json!({"step": 3}))
        );
        assert_eq!(
            parse(&["replay", "speed", "2"]).unwrap().command,
            invoke("replay.speed", serde_json::json!({"speed": 2.0}))
        );
        assert_eq!(
            parse(&["replay", "next"]).unwrap().command,
            invoke("replay.step.next", serde_json::json!({}))
        );
        assert_eq!(
            parse(&["replay", "status"]).unwrap().command,
            Command::ReplayStatus
        );
        for bad in [
            &["replay"][..],
            &["replay", "seek", "x"],
            &["replay", "open"],
            &["replay", "rewind"],
        ] {
            assert!(matches!(parse(bad), Err(ParseError::Usage(_))), "{bad:?}");
        }
    }

    #[test]
    fn feedback_verbs_are_the_shell_recording_actions() {
        let invoke = |id: &str, args: serde_json::Value| Command::ActionsInvoke {
            provider: SHELL_ACTIONS.into(),
            id: id.into(),
            args_json: Some(args.to_string()),
        };
        assert_eq!(
            parse(&[
                "feedback",
                "record",
                "start",
                "--room",
                "a",
                "--room",
                "b",
                "--app",
                "so.tilo.notes",
            ])
            .unwrap()
            .command,
            invoke(
                "feedback.record.start",
                serde_json::json!({"rooms": "a,b", "apps": "so.tilo.notes"})
            )
        );
        assert_eq!(
            parse(&["feedback", "record", "stop"]).unwrap().command,
            invoke("feedback.record.stop", serde_json::json!({}))
        );
        assert_eq!(
            parse(&["feedback", "record", "capture-recent"])
                .unwrap()
                .command,
            invoke("feedback.record.capture-recent", serde_json::json!({}))
        );
        assert_eq!(
            parse(&["feedback", "recent-buffer", "on"]).unwrap().command,
            invoke(
                "feedback.recent-buffer.set",
                serde_json::json!({"enabled": true})
            )
        );
        assert_eq!(
            parse(&["feedback", "status"]).unwrap().command,
            Command::FeedbackStatus
        );
        for bad in [
            &["feedback"][..],
            &["feedback", "record"],
            &["feedback", "record", "start", "--room"],
            &["feedback", "recent-buffer", "maybe"],
        ] {
            assert!(matches!(parse(bad), Err(ParseError::Usage(_))), "{bad:?}");
        }
    }

    #[test]
    fn inspect_queries_reach_the_wire_as_typed_inspect_queries() {
        let wire = |args: &[&str]| -> serde_json::Value {
            serde_json::from_str(&command_to_wire(&parse(args).unwrap().command).unwrap()).unwrap()
        };
        assert_eq!(wire(&["inspect", "apps"])["op"], "inspect_apps");
        let at = wire(&["inspect", "at", "7", "12.5", "40"]);
        assert_eq!(at["op"], "inspect");
        assert_eq!(at["peer"], 7);
        assert!(at["surface"].is_null());
        let window = wire(&["inspect", "tree", "surface:12"]);
        assert_eq!(window["surface"], 12);
        assert!(window["peer"].is_null());
        assert_eq!(at["local"], false);
        let query: ui::inspect::InspectQuery = serde_json::from_value(at["query"].clone()).unwrap();
        assert_eq!(query, ui::inspect::InspectQuery::At { x: 12.5, y: 40.0 });
        let node = wire(&["inspect", "node", "7", "t:save", "--local"]);
        assert_eq!(node["local"], true);
        let query: ui::inspect::InspectQuery =
            serde_json::from_value(node["query"].clone()).unwrap();
        assert_eq!(
            query,
            ui::inspect::InspectQuery::Node {
                id: "t:save".into()
            }
        );
        let tree = wire(&["inspect", "tree", "7", "--root", "t:form", "--depth", "3"]);
        let query: ui::inspect::InspectQuery =
            serde_json::from_value(tree["query"].clone()).unwrap();
        assert_eq!(
            query,
            ui::inspect::InspectQuery::Subtree {
                root: Some("t:form".into()),
                depth: 3
            }
        );
    }

    #[test]
    fn inspect_explains_a_missing_or_malformed_peer() {
        for args in [
            &["inspect"][..],
            &["inspect", "at"],
            &["inspect", "at", "seven", "1", "2"],
            &["inspect", "at", "7", "1"],
            &["inspect", "tree", "7", "--depth"],
            &["inspect", "node", "7", "a", "b"],
        ] {
            let Err(ParseError::Usage(message)) = parse(args) else {
                panic!("{args:?} must be a usage error");
            };
            assert!(!message.is_empty());
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

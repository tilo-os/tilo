//! `qos-control` — thin CLI over the `QOS_TEST_CONTROL` socket.
//!
//! Wraps the JSON command protocol of [`shell::compositor::test_control`] so
//! scripts don't have to glue `socat` to `jq`. The socket path comes from
//! `$QOS_TEST_CONTROL` — the same env var the compositor binds against —
//! falling back to `/tmp/nirios.sock`. No `clap`, no async, just stdlib +
//! `serde_json`. Run `--help` for the subcommand list.
//!
//! Exit codes: `0` on `Ok`/`Targets`/`State`, `1` on `Err` (echoing
//! `err.code`/`err.message` to stderr), `2` on argv / I/O / parse failures.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

/// Default socket path when neither `--socket` nor `$QOS_TEST_CONTROL`
/// is set. Production runs never bind here because the env-var gate
/// inside the compositor refuses to listen without a path.
const DEFAULT_SOCKET: &str = "/tmp/nirios.sock";

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
    },
    Raw {
        json: String,
    },
}

/// Top-level CLI invocation — socket path + the parsed subcommand.
#[derive(Debug, Clone, PartialEq)]
struct Invocation {
    socket: PathBuf,
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
fn parse_global_flags(argv: &[String]) -> Result<(PathBuf, Vec<String>), ParseError> {
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
    let socket = socket
        .or_else(|| std::env::var_os("QOS_TEST_CONTROL").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
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
fn parse_actions_command<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<Command, ParseError> {
    let verb = iter.next().map(String::as_str).ok_or_else(|| {
        ParseError::Usage("actions requires `list` or `invoke` (try --help)".to_string())
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
            Ok(Command::ActionsInvoke {
                provider: provider.clone(),
                id: id.clone(),
            })
        }
        other => Err(ParseError::Usage(format!(
            "actions: unknown verb `{other}` (expected `list` or `invoke`)"
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
            if let Some(s) = steps {
                v["steps"] = json!(s);
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
        Command::StateDump { .. } => json!({ "op": "state_dump" }),
        Command::ActionsList { query, .. } => {
            json!({ "op": "actions_enumerate", "query": query })
        }
        Command::ActionsInvoke { provider, id } => {
            json!({ "op": "actions_invoke", "provider": provider, "id": id })
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
        Command::ActionsInvoke { .. } => {
            // Echo what the address resolved to, so an operator can see
            // *which* action ran rather than a bare "ok".
            let answer = response
                .get("answer")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let title = answer.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let provider = answer
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let id = answer.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            println!("invoked {provider}:{id}  {title}");
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
        "qos-control — drive a running quick-desktop's test_control socket\n\
         \n\
         Usage:\n\
         \x20 qos-control [--socket PATH] <subcommand> [args]\n\
         \n\
         Default socket path comes from $QOS_TEST_CONTROL, falling back to\n\
         {DEFAULT_SOCKET}.\n\
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
         \x20 actions invoke PROVIDER ID    run one action by address, with no synthetic input\n\
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
            eprintln!("qos-control: {msg}");
            return ExitCode::from(2);
        }
    };

    let cmd_json = match command_to_wire(&invocation.command) {
        Ok(j) => j,
        Err(ParseError::Help) => unreachable!(),
        Err(ParseError::Usage(msg)) => {
            eprintln!("qos-control: {msg}");
            return ExitCode::from(2);
        }
    };

    let mut stream = match connect(&invocation.socket) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("qos-control: connect {:?}: {e}", invocation.socket);
            return ExitCode::from(2);
        }
    };

    let response = match round_trip(&mut stream, &cmd_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("qos-control: socket round-trip: {e}");
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
        let argv: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
        parse_args(&argv)
    }

    #[test]
    fn list_targets_parses_with_no_args() {
        let inv = parse(&["list-targets"]).unwrap();
        assert_eq!(inv.command, Command::ListTargets);
    }

    #[test]
    fn click_parses_name_argument() {
        let inv = parse(&["click", "btn"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Click {
                name: "btn".to_string()
            }
        );
    }

    #[test]
    fn click_without_name_errors() {
        let err = parse(&["click"]).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    #[test]
    fn click_at_parses_two_floats() {
        let inv = parse(&["click-at", "100.5", "200"]).unwrap();
        assert_eq!(inv.command, Command::ClickAt { x: 100.5, y: 200.0 });
    }

    #[test]
    fn click_at_with_bad_number_errors() {
        let err = parse(&["click-at", "nope", "200"]).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    #[test]
    fn drag_parses_four_floats_with_optional_steps() {
        let inv = parse(&["drag", "0", "0", "100", "100"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Drag {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 100.0,
                steps: None,
            }
        );

        let inv = parse(&["drag", "0", "0", "100", "100", "16"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Drag {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 100.0,
                steps: Some(16),
            }
        );
    }

    #[test]
    fn key_parses_keyword_argument() {
        let inv = parse(&["key", "Enter"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Key {
                key: "Enter".to_string()
            }
        );
    }

    #[test]
    fn type_parses_text_with_spaces_as_single_arg() {
        // The OS arg-tokenizer hands us the already-quoted string,
        // so `type "hello world"` arrives as one argv slot.
        let inv = parse(&["type", "hello world"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Type {
                text: "hello world".to_string()
            }
        );
    }

    #[test]
    fn modifiers_parses_comma_separated_flags() {
        let inv = parse(&["modifiers", "ctrl,shift"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
                logo: false,
            }
        );
    }

    #[test]
    fn modifiers_empty_string_clears_all() {
        let inv = parse(&["modifiers", ""]).unwrap();
        assert_eq!(
            inv.command,
            Command::Modifiers {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
            }
        );
    }

    #[test]
    fn modifiers_unknown_flag_errors() {
        let err = parse(&["modifiers", "meta"]).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    #[test]
    fn snapshot_parses_path_argument() {
        let inv = parse(&["snapshot", "/tmp/x.png"]).unwrap();
        assert_eq!(
            inv.command,
            Command::Snapshot {
                path: "/tmp/x.png".to_string()
            }
        );
    }

    #[test]
    fn state_dump_parses_with_no_args() {
        let inv = parse(&["state-dump"]).unwrap();
        assert_eq!(inv.command, Command::StateDump { pretty: false });
    }

    #[test]
    fn state_dump_pretty_flag() {
        let inv = parse(&["state-dump", "--pretty"]).unwrap();
        assert_eq!(inv.command, Command::StateDump { pretty: true });
    }

    #[test]
    fn raw_parses_json_payload() {
        let inv = parse(&["raw", r#"{"op":"list_targets"}"#]).unwrap();
        assert_eq!(
            inv.command,
            Command::Raw {
                json: r#"{"op":"list_targets"}"#.to_string()
            }
        );
    }

    #[test]
    fn unknown_subcommand_errors() {
        let err = parse(&["go-fishing"]).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    #[test]
    fn help_flag_returns_help_variant() {
        let err = parse(&["--help"]).unwrap_err();
        assert_eq!(err, ParseError::Help);
        let err = parse(&["-h"]).unwrap_err();
        assert_eq!(err, ParseError::Help);
    }

    #[test]
    fn socket_flag_overrides_default() {
        let inv = parse(&["--socket", "/custom/path", "list-targets"]).unwrap();
        assert_eq!(inv.socket, PathBuf::from("/custom/path"));
    }

    #[test]
    fn socket_flag_with_missing_path_errors() {
        let err = parse(&["--socket"]).unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    #[test]
    fn default_socket_used_when_env_unset() {
        // SAFETY: clearing the env-var has process-wide effect — the
        // test runner uses `--test-threads 1` in CI, and within a
        // single thread the assertion holds. Other unit tests in the
        // crate don't read this variable.
        let prev = std::env::var_os("QOS_TEST_CONTROL");
        unsafe {
            std::env::remove_var("QOS_TEST_CONTROL");
        }
        let inv = parse(&["list-targets"]).unwrap();
        // Restore before asserting so a panic can't poison the env for later tests.
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("QOS_TEST_CONTROL", v);
            }
        }
        assert_eq!(inv.socket, PathBuf::from(DEFAULT_SOCKET));
    }

    // ===================================================================
    // Wire payload encoding — guards the JSON shape per subcommand.
    // ===================================================================

    #[test]
    fn wire_list_targets_emits_op_only() {
        let json = command_to_wire(&Command::ListTargets).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "list_targets");
    }

    #[test]
    fn wire_click_emits_click_named_with_name() {
        let json = command_to_wire(&Command::Click {
            name: "btn".to_string(),
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "click_named");
        assert_eq!(v["name"], "btn");
    }

    #[test]
    fn wire_click_at_emits_x_y() {
        let json = command_to_wire(&Command::ClickAt { x: 1.5, y: 2.5 }).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "click_at");
        assert_eq!(v["x"], 1.5);
        assert_eq!(v["y"], 2.5);
    }

    #[test]
    fn wire_drag_includes_steps_when_set_omits_when_unset() {
        let json = command_to_wire(&Command::Drag {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
            steps: None,
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "drag");
        assert!(v.get("steps").is_none(), "steps must be absent when None");

        let json = command_to_wire(&Command::Drag {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
            steps: Some(4),
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["steps"], 4);
    }

    #[test]
    fn wire_modifiers_emits_four_bool_flags() {
        let json = command_to_wire(&Command::Modifiers {
            ctrl: true,
            shift: false,
            alt: true,
            logo: false,
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "modifiers");
        assert_eq!(v["ctrl"], true);
        assert_eq!(v["shift"], false);
        assert_eq!(v["alt"], true);
        assert_eq!(v["logo"], false);
    }

    #[test]
    fn wire_state_dump_emits_op_only() {
        let json = command_to_wire(&Command::StateDump { pretty: true }).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["op"], "state_dump");
        // `pretty` is purely a print-time concern; the wire payload
        // never carries it (the compositor doesn't need to know).
        assert!(v.get("pretty").is_none());
    }

    #[test]
    fn wire_raw_passes_through_verbatim() {
        let json = command_to_wire(&Command::Raw {
            json: r#"{"op":"list_targets","extra":1}"#.to_string(),
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"list_targets","extra":1}"#);
    }

    #[test]
    fn wire_raw_with_invalid_json_errors() {
        let err = command_to_wire(&Command::Raw {
            json: "not json".to_string(),
        })
        .unwrap_err();
        assert!(matches!(err, ParseError::Usage(_)), "{err:?}");
    }

    // ===================================================================
    // Modifier-flag parser
    // ===================================================================

    #[test]
    fn parse_modifier_flags_empty_string_returns_all_false() {
        assert_eq!(
            parse_modifier_flags("").unwrap(),
            (false, false, false, false)
        );
    }

    #[test]
    fn parse_modifier_flags_all_four_set() {
        assert_eq!(
            parse_modifier_flags("ctrl,shift,alt,logo").unwrap(),
            (true, true, true, true)
        );
    }

    #[test]
    fn parse_modifier_flags_trims_whitespace() {
        // Tolerate `ctrl, shift` (with a space) — shells often
        // produce that when the user quotes the arg.
        assert_eq!(
            parse_modifier_flags("ctrl, shift").unwrap(),
            (true, true, false, false)
        );
    }
}

//! `cargo daemon`: this checkout's device daemon in the background, over a profile of its own,
//! for the command line, the SDKs and agents to reach while a developer works on Tilo.
//!
//! It drives the path that already exists for a daemon named apart: `tilo-runtime --profile DIR`
//! (crates/core/tilo-runtime/src/cli.rs), whose clients find it through `TILO_RUNTIME_PROFILE`,
//! and which the development sessions already start. What this adds is the state layout that
//! keeps it clear of the person's own daemon (profile, identity, workspace folders, sockets), the
//! line saying how to reach it and how to stop it, `--stop`, and `--check`: a smoke test that
//! starts it, asks it something, and stops it.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::{json, repo_root, seconds, state_home, target_dir};

const HELP: &str = "\
cargo daemon: a Tilo device daemon in the background, from this checkout.

Builds this checkout's daemon and starts it over a profile of its own under a
state directory, so it runs beside the daemon this computer runs: neither its
profile, its sockets, its identity nor its workspaces' folders are that one's,
and the person's own daemon keeps running untouched. It is a plain detached
process with a pid file, so it works on any Linux.

On a fresh profile the daemon founds a throwaway person of its own, as `cargo
headless` does. With --ticket it becomes a device of an existing person
instead, with a ticket `tilo device add` printed on one of their devices
(`tilo login`). It runs no shell and no desktop: it serves the action model,
the device and the person's workspaces to the command line, the SDKs and
agents that name its profile.

Once it answers, it prints the lines to paste into a shell:

  export TILO_RUNTIME_PROFILE=...
  export TILO_IDENTITY_ROOT=...

Usage: cargo daemon [OPTIONS]

Options:
  --state DIR         state directory (default: $XDG_STATE_HOME/tilo-daemon-dev);
                      its profile is DIR/profile, its identity DIR/identity and
                      its workspaces' folders DIR/home/<workspace>
  --fresh             discard the state, its person and its workspaces, and
                      start again
  --ticket TICKET     log in as a device of an existing person instead of
                      founding one: the ticket `tilo device add` printed on one
                      of their devices
  --no-build          run the last build (target/debug) as it is
  --stop              stop the daemon this state directory started, and exit
  --check             smoke test: start it on a fresh state, ask it `tilo status
                      --daemon` and one read action (devices#info), stop it, and
                      say whether it worked; founds its own person, so it takes
                      no --ticket

Logs: STATE/logs/daemon.log.

Examples:
  cargo daemon                     # then paste the export lines it prints
  cargo daemon --fresh
  cargo daemon --stop
  cargo daemon --check
";

/// The profile inside the state directory: `TILO_RUNTIME_PROFILE`.
const PROFILE_DIR: &str = "profile";
/// `HOME` for the daemon: its workspaces' folders are at `DIR/<workspace>` below it, never at the
/// person's own home, so the two daemons' folders never meet.
const HOME_DIR: &str = "home";
/// `TILO_IDENTITY_ROOT` for the daemon: its device key, its certificates and its person's root,
/// so it never reads or writes the identity of the computer's own daemon.
const IDENTITY_DIR: &str = "identity";
/// The daemon's log, appended to across starts.
const LOG_FILE: &str = "logs/daemon.log";
/// The pid of the daemon this state directory started, for `--stop`.
const PID_FILE: &str = "daemon.pid";
/// The lock the daemon holds on its profile for as long as it serves it, inside the profile: how
/// `--stop` finds the daemon while the daemon keeps itself undumpable. Named as
/// `tilo_runtime::profile::lock` names it.
const PROFILE_LOCK: &str = "runtime.lock";

/// The line the daemon answers with its status, as `tilo status --daemon` asks it: what says the
/// daemon answers, not a socket it left behind.
const STATUS_LINE: &str = r#"{"native":"status"}"#;

/// What the daemon is asked with [`founded`] refused because the profile's device is one of a
/// person's already: the state was founded before, and is kept.
const ALREADY_A_PERSONS: &str = "this device is a person's already";

/// How long each step of a founding call is waited for before it is asked about again, as
/// `tilo`'s own first command follows one.
const FOUNDING_STEP_MS: u64 = 55_000;

/// How long a fresh daemon may take to answer: it opens its stores, its stores' locks and its
/// endpoint, on a machine that may be busy with the build it just ran.
const STARTING: Duration = Duration::from_secs(180);
/// How long founding the profile's person may take: it starts their services first.
const FOUNDING: Duration = Duration::from_secs(300);
/// How long a daemon told to stop may take before it is killed. It gives up on each of its steps
/// in turn, and its own waits add up to less than this.
const STOPPING: Duration = Duration::from_secs(45);
/// How long the check waits for an answer to one question.
const ASKING: Duration = Duration::from_secs(10);

/// What the command line asks for.
struct Options {
    state: PathBuf,
    run: Run,
    build: bool,
    fresh: bool,
    ticket: Option<String>,
}

/// What to do with the daemon.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    /// Start it, or say where it already answers.
    Start,
    /// Stop the one this state started.
    Stop,
    /// Smoke-test it.
    Check,
}

fn parse(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        state: state_home("tilo-daemon-dev"),
        run: Run::Start,
        build: true,
        fresh: false,
        ticket: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value (cargo daemon --help)"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--state" => options.state = PathBuf::from(value("--state")?),
            "--ticket" => options.ticket = Some(value("--ticket")?),
            "--no-build" => options.build = false,
            "--fresh" => options.fresh = true,
            "--check" => options.run = Run::Check,
            "--stop" => options.run = Run::Stop,
            other => {
                return Err(format!(
                    "unknown option {other} (cargo daemon --help lists them)"
                ));
            }
        }
    }
    if options.run == Run::Check && options.ticket.is_some() {
        return Err("--check founds its own person on a fresh state: it takes no --ticket".into());
    }
    if !options.state.is_absolute() {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        options.state = cwd.join(&options.state);
    }
    Ok(Some(options))
}

pub fn main(args: Vec<String>) -> Result<ExitCode, String> {
    let Some(options) = parse(args)? else {
        return Ok(ExitCode::SUCCESS);
    };
    if options.run == Run::Stop {
        return Ok(stop(&options.state));
    }
    let repo = repo_root()?;
    if options.run == Run::Check {
        return check(&repo, &options);
    }
    run(&repo, &options)
}

/// Start the daemon, or say where the one this state started already answers.
fn run(repo: &Path, options: &Options) -> Result<ExitCode, String> {
    let began = Instant::now();
    let state = &options.state;
    let profile = state.join(PROFILE_DIR);
    let socket = native_socket(&profile);
    // A start over ends the daemon this state started before it wipes what that daemon kept;
    // without it, a state with a daemon of its own answers where it stands.
    if options.fresh {
        end_daemon(state, &profile);
    } else if let Some(pid) = live_pid(state, &profile) {
        eprintln!(
            "A development daemon already answers from {} (pid {pid}).",
            state.display()
        );
        print!("{}", reach(state, &profile, began.elapsed(), Reach::Kept));
        return Ok(ExitCode::SUCCESS);
    }
    // A daemon this command did not start (its pid file is lost) is left alone, and so is the
    // state under it: --fresh must never wipe a daemon's state while it runs.
    if answers(&socket) {
        let hint = if options.fresh {
            ", before --fresh, which would wipe the state under it"
        } else {
            ", or choose another --state"
        };
        return Err(format!(
            "a daemon this command did not start already serves {}{hint}",
            profile.display()
        ));
    }
    if options.fresh {
        fs::remove_dir_all(state)
            .or_else(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(error),
            })
            .map_err(|error| format!("{}: {error}", state.display()))?;
    }
    prepare(state)?;
    if options.build {
        build(repo, options.ticket.is_some())?;
    }
    let mut child = start(repo, state, &profile).map_err(|error| {
        format!(
            "{error} (the state is {}; remove it with --fresh)",
            state.display()
        )
    })?;
    if let Err(error) = wait_answering(&socket, &mut child, STARTING, state) {
        signal_group(child.id(), "TERM");
        return Err(error);
    }
    let reached = match &options.ticket {
        Some(ticket) => {
            log_in(repo, state, &profile, ticket)?;
            Reach::Ticket
        }
        None => {
            if founded(&socket)? {
                Reach::Founded
            } else {
                Reach::Kept
            }
        }
    };
    print!("{}", reach(state, &profile, began.elapsed(), reached));
    Ok(ExitCode::SUCCESS)
}

/// What the daemon keeps as this state's person, as [`reach`] says it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Founded now, by this start: a throwaway person of this checkout's.
    Founded,
    /// The person founded by an earlier start, whose workspaces and identity are kept.
    Kept,
    /// Logged in with a ticket: a device of an existing person.
    Ticket,
}

/// How to reach the daemon and how to stop it: the lines a person pastes.
fn reach(state: &Path, profile: &Path, took: Duration, reached: Reach) -> String {
    let person = match reached {
        Reach::Founded => {
            "Its person is its own, founded fresh now: not yours, and its workspaces are empty."
        }
        Reach::Kept => "It keeps the person and workspaces an earlier start founded.",
        Reach::Ticket => "It is a device of the person the ticket logged in.",
    };
    let stop = if state == default_state().as_path() {
        "cargo daemon --stop".to_owned()
    } else {
        format!("cargo daemon --stop --state {}", quoted(state))
    };
    format!(
        "\n==> Tilo's development daemon is up ({} to start).\n\
         \x20   {person}\n\
         \x20   state:     {state}\n\
         \x20   logs:      {log}\n\
         \x20   folders:   {folders}/<workspace>, the workspaces it keeps\n\
         To use it from this shell, paste:\n\n\
         \x20   export TILO_RUNTIME_PROFILE={profile}\n\
         \x20   export TILO_IDENTITY_ROOT={identity}\n\n\
         Stop it with: {stop}\n\n",
        seconds(took),
        state = state.display(),
        log = state.join(LOG_FILE).display(),
        folders = state.join(HOME_DIR).display(),
        profile = quoted(profile),
        identity = quoted(&state.join(IDENTITY_DIR)),
    )
}

/// `path` as a shell pastes it: single-quoted, quotes inside it closed and reopened.
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The state directory a start without `--state` uses.
fn default_state() -> PathBuf {
    state_home("tilo-daemon-dev")
}

/// The state's own directories, private to this account as the daemon's profile must be.
fn prepare(state: &Path) -> Result<(), String> {
    for dir in [state.to_path_buf(), state.join("logs")] {
        private_dir(&dir)?;
    }
    for dir in [PROFILE_DIR, HOME_DIR, IDENTITY_DIR] {
        private_dir(&state.join(dir))?;
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Build the programs a start runs: the daemon, and the `tilo` command when a login or the check
/// asks the daemon something through it.
fn build(repo: &Path, cli: bool) -> Result<(), String> {
    cargo(
        repo,
        &[
            "build",
            "--locked",
            "-p",
            "tilo-daemon",
            "--bin",
            "tilo-runtime",
        ],
    )?;
    if cli {
        cargo(
            repo,
            &["build", "--locked", "-p", "tilo-cli", "--bin", "tilo"],
        )?;
    }
    Ok(())
}

fn cargo(repo: &Path, args: &[&str]) -> Result<(), String> {
    eprintln!("==> cargo {}", args.join(" "));
    let status = Command::new("cargo")
        .current_dir(repo)
        .args(args)
        .status()
        .map_err(|error| format!("cargo: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("the build failed (above)".into())
    }
}

/// Start the daemon over `profile` in the background: a process of its own, its output in the
/// state's log, and its pid written down for `--stop`.
fn start(repo: &Path, state: &Path, profile: &Path) -> Result<Child, String> {
    let binary = target_dir(repo).join("debug/tilo-runtime");
    if !binary.is_file() {
        return Err(format!("no {} (run without --no-build)", binary.display()));
    }
    let log_path = state.join(LOG_FILE);
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("{}: {error}", log_path.display()))?;
    writeln!(
        log,
        "\n==> {} starting over {}",
        binary.display(),
        profile.display()
    )
    .map_err(|error| format!("{}: {error}", log_path.display()))?;
    let errors = log
        .try_clone()
        .map_err(|error| format!("{}: {error}", log_path.display()))?;
    let mut child = Command::new(&binary)
        .arg("--profile")
        .arg(profile)
        .env("TILO_RUNTIME_PROFILE", profile)
        .env("TILO_IDENTITY_ROOT", state.join(IDENTITY_DIR))
        .env("HOME", state.join(HOME_DIR))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("TILO_TEST_CONTROL")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors))
        .process_group(0)
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", binary.display()))?;
    let pidfile = state.join(PID_FILE);
    if let Err(error) = fs::write(&pidfile, format!("{}\n", child.id())) {
        signal_group(child.id(), "TERM");
        return Err(format!("{}: {error}", pidfile.display()));
    }
    child
        .try_wait()
        .map_err(|error| error.to_string())?
        .map_or(Ok(child), |status| {
            Err(format!(
                "the daemon ended as it started ({status}); see {}",
                log_path.display()
            ))
        })
}

/// Where the daemon serving `profile` listens: its profile's directory among this account's
/// sockets, named by the profile's path as every client of it names it
/// (`tilo_device_identity::login::daemon::profile_sockets`).
fn native_socket(profile: &Path) -> PathBuf {
    let uid = fs::metadata("/proc/self").map_or(0, |meta| meta.uid());
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .or_else(|| {
            let run = PathBuf::from(format!("/run/user/{uid}"));
            run.is_dir().then_some(run)
        })
        .map_or_else(
            || std::env::temp_dir().join(format!("tilo-{uid}")),
            |runtime| runtime.join("tilo"),
        );
    base.join(profile_socket_name(profile)).join("native.sock")
}

/// A profile's name among this account's sockets: FNV-1a over its path, as every client of a
/// daemon named apart computes it.
fn profile_socket_name(profile: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in profile.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("profile-{hash:016x}")
}

/// Wait until the daemon answers at `socket`, and fail naming its log when it ended or took
/// longer than `limit`.
fn wait_answering(
    socket: &Path,
    child: &mut Child,
    limit: Duration,
    state: &Path,
) -> Result<(), String> {
    let began = Instant::now();
    loop {
        if answers(socket) {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "the daemon ended as it started ({status}){}",
                log_tail(state)
            ));
        }
        if began.elapsed() > limit {
            return Err(format!(
                "the daemon did not answer on {} within {}{}",
                socket.display(),
                seconds(limit),
                log_tail(state)
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The last lines of the state's log, for an error that names what the daemon said.
fn log_tail(state: &Path) -> String {
    let log = state.join(LOG_FILE);
    let Ok(text) = fs::read_to_string(&log) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().rev().take(20).collect();
    let mut said = format!(" (see {}):", log.display());
    for line in lines.iter().rev() {
        said.push_str("\n  ");
        said.push_str(line);
    }
    said
}

/// Whether the daemon answers at `socket`.
fn answers(socket: &Path) -> bool {
    ask(socket, STATUS_LINE, ASKING).is_ok_and(|answer| !answer.trim().is_empty())
}

/// Ask the daemon at `socket` one line, and read its answer.
fn ask(socket: &Path, line: &str, within: Duration) -> Result<String, String> {
    let mut stream =
        tilo_socket::connect(socket).map_err(|error| format!("{}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(within))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(within))
        .map_err(|error| error.to_string())?;
    writeln!(stream, "{line}").map_err(|error| format!("{}: {error}", socket.display()))?;
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(|error| format!("the daemon did not answer: {error}"))?;
    if answer.trim().is_empty() {
        return Err("the daemon answered nothing".into());
    }
    Ok(answer)
}

/// Found the profile's own person through the daemon, as `tilo`'s first command does: whether a
/// person was founded now, `false` when the state was founded before and is kept.
///
/// A founding call is answered in parts, each stage reported while the person's services start,
/// so it is followed until it settles: the daemon serves its person only once it has. A state
/// founded before refuses the call, and is kept.
fn founded(socket: &Path) -> Result<bool, String> {
    let began = Instant::now();
    let stream =
        tilo_socket::connect(socket).map_err(|error| format!("{}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(FOUNDING))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(FOUNDING))
        .map_err(|error| error.to_string())?;
    let mut writing = stream
        .try_clone()
        .map_err(|error| format!("{}: {error}", socket.display()))?;
    let mut reading = BufReader::new(stream);
    let mut id = 0_u64;
    let mut following: Option<String> = None;
    loop {
        if began.elapsed() > FOUNDING {
            return Err(format!(
                "the profile's person did not finish founding within {}",
                seconds(FOUNDING)
            ));
        }
        id = id.saturating_add(1);
        let request = match &following {
            // The throwaway person of a development daemon: `devices#found`, with no name and no
            // tickets of their first device.
            None => format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"tilo_do","arguments":{{"path":"devices#found","wait_ms":{FOUNDING_STEP_MS},"args":{{}}}}}}}}"#
            ),
            // The stage the founding call is in, asked about as `tilo` follows its own.
            Some(invocation) => format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"tilo_status","arguments":{{"invocation_id":"{invocation}","wait_ms":{FOUNDING_STEP_MS}}}}}}}"#
            ),
        };
        writeln!(writing, "{request}").map_err(|error| format!("{}: {error}", socket.display()))?;
        let mut answer = String::new();
        reading
            .read_line(&mut answer)
            .map_err(|error| format!("the daemon did not answer: {error}"))?;
        if answer.trim().is_empty() {
            return Err("the daemon answered nothing".into());
        }
        let text = json::string_field(&answer, "text").unwrap_or_default();
        if let Some(pending) = text.trim().strip_prefix("pending ") {
            let invocation = pending
                .split_once(':')
                .map_or(pending, |(invocation, _)| invocation)
                .trim();
            if invocation.is_empty()
                || !invocation
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(format!(
                    "the daemon named no founding call: {}",
                    answer.trim()
                ));
            }
            following = Some(invocation.to_owned());
            continue;
        }
        let refused = answer.replace(' ', "").contains("\"isError\":true");
        if answer.contains(ALREADY_A_PERSONS) {
            return Ok(false);
        }
        if refused || answer.contains("\"error\"") {
            return Err(format!(
                "the daemon founded no person: {text}{}",
                answer.trim()
            ));
        }
        return Ok(true);
    }
}

/// Run `tilo login` against the daemon: the ticket's person certifies the daemon's device.
fn log_in(repo: &Path, state: &Path, profile: &Path, ticket: &str) -> Result<(), String> {
    let tilo = target_dir(repo).join("debug/tilo");
    if !tilo.is_file() {
        return Err(format!("no {} (run without --no-build)", tilo.display()));
    }
    let status = Command::new(&tilo)
        .args(["login", ticket])
        .env("TILO_RUNTIME_PROFILE", profile)
        .env("TILO_IDENTITY_ROOT", state.join(IDENTITY_DIR))
        .env("HOME", state.join(HOME_DIR))
        .status()
        .map_err(|error| format!("could not run {}: {error}", tilo.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "tilo login failed ({status}); the state is {} (remove it with --fresh)",
            state.display()
        ))
    }
}

/// `cargo daemon --stop`: stop the daemon this state started.
fn stop(state: &Path) -> ExitCode {
    let profile = state.join(PROFILE_DIR);
    if end_daemon(state, &profile) {
        println!("Stopped the development daemon from {}.", state.display());
    } else {
        println!(
            "No development daemon is running from {} (nothing to stop).",
            state.display()
        );
    }
    ExitCode::SUCCESS
}

/// Stop the daemon this state started, and whether one was running: only the daemon that holds
/// the lock on this state's profile is signalled, never a pid that was reused or another daemon.
fn end_daemon(state: &Path, profile: &Path) -> bool {
    let pidfile = state.join(PID_FILE);
    let Some(pid) = read_pid(&pidfile) else {
        let _ = fs::remove_file(&pidfile);
        return false;
    };
    if !is_our_daemon(pid, profile) {
        let _ = fs::remove_file(&pidfile);
        return false;
    }
    signal_group(pid, "TERM");
    let began = Instant::now();
    while began.elapsed() < STOPPING {
        if !is_our_daemon(pid, profile) {
            let _ = fs::remove_file(&pidfile);
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    signal_group(pid, "KILL");
    let _ = fs::remove_file(&pidfile);
    true
}

/// The pid in `path`, when it holds one.
fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The daemon this state started, when it runs still.
fn live_pid(state: &Path, profile: &Path) -> Option<u32> {
    let pid = read_pid(&state.join(PID_FILE))?;
    is_our_daemon(pid, profile).then_some(pid)
}

/// Whether process `pid` is this state's daemon over `profile`: a daemon holds the lock it takes
/// on the profile for its whole life, and the pid that daemon recorded there is the one this state
/// wrote down.
///
/// A daemon that keeps this computer's device key makes itself undumpable, so neither its
/// environment nor its command line says anything about it. The lock does: it says a daemon serves
/// the profile, and which process it is. A pid the system reused, or another profile's daemon, is
/// not it.
fn is_our_daemon(pid: u32, profile: &Path) -> bool {
    let lock = profile.join(PROFILE_LOCK);
    if !lock.is_file() {
        return false;
    }
    match advisory_lock::FileLock::try_open(&lock, advisory_lock::LockMode::Exclusive) {
        // Free, and so taken here and let go at once: no daemon serves the profile.
        Ok(free) => {
            drop(free);
            false
        }
        Err(advisory_lock::TryLockError::WouldBlock) => {
            // A daemon starting holds the lock a moment before it records its pid; until it does,
            // the daemon this state started is the one it is.
            advisory_lock::owner_pid(&lock).is_none_or(|owner| owner == pid)
        }
        Err(advisory_lock::TryLockError::Error(_)) => false,
    }
}

/// Signal the process group `pid` leads (the daemon sets its own), by signal name.
fn signal_group(pid: u32, signal: &str) -> bool {
    Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// `cargo daemon --check`: start a fresh daemon, ask it `tilo status --daemon` and one read
/// action, stop it, and say whether all of that worked.
fn check(repo: &Path, asked: &Options) -> Result<ExitCode, String> {
    let began = Instant::now();
    let state = target_dir(repo).join("tilo-local-check/daemon");
    let profile = state.join(PROFILE_DIR);
    end_daemon(&state, &profile);
    fs::remove_dir_all(&state)
        .or_else(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(error),
        })
        .map_err(|error| format!("{}: {error}", state.display()))?;
    prepare(&state)?;
    let mut timings = Vec::new();
    if asked.build {
        build(repo, true)?;
        timings.push(format!("build {}", seconds(began.elapsed())));
    }
    let mut child = start(repo, &state, &profile)?;
    let outcome = probe(repo, &state, &profile, &mut child, &mut timings);
    let stopped = end_daemon(&state, &profile);
    verdict(outcome, stopped, &state, &timings)
}

/// Ask the running daemon what it is.
fn probe(
    repo: &Path,
    state: &Path,
    profile: &Path,
    child: &mut Child,
    timings: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let started = Instant::now();
    let socket = native_socket(profile);
    wait_answering(&socket, child, STARTING, state)?;
    timings.push(format!("start {}", seconds(started.elapsed())));
    if !founded(&socket)? {
        return Err(format!(
            "the check's fresh state was a person's already ({}); remove it and ask again",
            state.display()
        ));
    }
    let mut said = vec![format!(
        "a person founded on a fresh profile in {}",
        seconds(started.elapsed())
    )];
    let asked = Instant::now();
    let status = wait_status(repo, state, profile)?;
    said.push(format!("tilo status --daemon: {}", status.trim()));
    said.extend(device_info(repo, state, profile)?);
    timings.push(format!("ask {}", seconds(asked.elapsed())));
    Ok(said)
}

/// `tilo-runtime --device-info`, the daemon's simplest read action (`devices#info`).
fn device_info(repo: &Path, state: &Path, profile: &Path) -> Result<Vec<String>, String> {
    let runtime = target_dir(repo).join("debug/tilo-runtime");
    let out = Command::new(&runtime)
        .arg("--device-info")
        .env("TILO_RUNTIME_PROFILE", profile)
        .env("TILO_IDENTITY_ROOT", state.join(IDENTITY_DIR))
        .env("HOME", state.join(HOME_DIR))
        .output()
        .map_err(|error| format!("{} --device-info: {error}", runtime.display()))?;
    if !out.status.success() {
        return Err(format!(
            "devices#info failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let device = json::string_field(text.trim(), "device")
        .ok_or_else(|| format!("devices#info named no device: {}", text.trim()))?;
    let shown: String = device.chars().take(16).collect();
    Ok(vec![format!("devices#info: device {shown}…")])
}

/// `tilo status --daemon`, asked again while the person's services start and the person's part
/// takes the endpoint over after founding: until it does, the daemon answers nothing there.
fn wait_status(repo: &Path, state: &Path, profile: &Path) -> Result<String, String> {
    let began = Instant::now();
    let limit = Duration::from_secs(300);
    loop {
        match run_cli(repo, state, profile, "status", &["--daemon"]) {
            Ok(said) => return Ok(said),
            Err(error) => {
                if began.elapsed() >= limit {
                    return Err(format!(
                        "the daemon answered nothing on its endpoint within {}: {error}",
                        seconds(limit)
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// The `tilo` command of this checkout, with the daemon's environment, answering `ARGS`.
fn run_cli(
    repo: &Path,
    state: &Path,
    profile: &Path,
    noun: &str,
    args: &[&str],
) -> Result<String, String> {
    let tilo = target_dir(repo).join("debug/tilo");
    if !tilo.is_file() {
        return Err(format!("no {} (run without --no-build)", tilo.display()));
    }
    let out = Command::new(&tilo)
        .arg(noun)
        .args(args)
        .env("TILO_RUNTIME_PROFILE", profile)
        .env("TILO_IDENTITY_ROOT", state.join(IDENTITY_DIR))
        .env("HOME", state.join(HOME_DIR))
        .output()
        .map_err(|error| format!("tilo {noun}: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "tilo {noun} failed ({}): {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn verdict(
    outcome: Result<Vec<String>, String>,
    stopped: bool,
    state: &Path,
    timings: &[String],
) -> Result<ExitCode, String> {
    let said = outcome.inspect_err(|_| {
        let tail = log_tail(state);
        if !tail.is_empty() {
            eprintln!("{tail}");
        }
    })?;
    for line in said {
        println!("ok: {line}");
    }
    if !stopped {
        return Err("the daemon did not stop".into());
    }
    println!("ok: the daemon stopped and let go of its state");
    println!("ok: cargo daemon --check passed ({})", timings.join(", "));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{PROFILE_LOCK, is_our_daemon, profile_socket_name};

    #[test]
    fn a_profile_is_named_among_the_accounts_sockets_by_its_path() {
        // `tilo_device_identity::login::daemon::profile_socket_name`, whose value every client of
        // a daemon named apart computes the same way.
        assert_eq!(
            profile_socket_name(Path::new("/home/u/.local/state/tilo-daemon-dev/profile")),
            "profile-12f6cc287a2860cb"
        );
    }

    /// The daemon a start leaves behind makes itself undumpable, so its environment and command
    /// line say nothing a stop can read: the lock it holds on its profile says which process
    /// serves the profile, and that is the one a stop signals.
    #[test]
    fn the_profiles_lock_names_the_daemon_that_serves_it() {
        let state =
            std::env::temp_dir().join(format!("tilo-local-stop-test-{}", std::process::id()));
        let profile = state.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        // A profile no daemon has served: nothing to stop, and no lock file made here.
        assert!(!is_our_daemon(std::process::id(), &profile));
        assert!(!profile.join(PROFILE_LOCK).exists());

        // A daemon serving it holds the profile's lock and records the pid it runs as.
        let serving = advisory_lock::FileLock::open(
            &profile.join(PROFILE_LOCK),
            advisory_lock::LockMode::Exclusive,
        )
        .unwrap();
        serving.resource_owner().unwrap();
        let daemon = std::process::id();
        assert!(is_our_daemon(daemon, &profile));
        // A pid the state wrote down that is not the lock holder's is never signalled.
        assert!(!is_our_daemon(daemon.saturating_add(1), &profile));

        // The daemon ended: the profile is free, and the record it left means nothing.
        drop(serving);
        assert!(!is_our_daemon(daemon, &profile));
        std::fs::remove_dir_all(&state).unwrap();
    }
}

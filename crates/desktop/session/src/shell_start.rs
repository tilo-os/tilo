//! A session started on a computer whose Tilo service runs as the person's own account never runs
//! as the person's shell itself: one a display manager starts (a distribution Tilo was added to),
//! and one a development script starts beside the service it started first. It asks that service
//! to start the shell (`shells#start`), which hands the shell the link it is admitted by, and
//! stands in for it until it ends: the signals sent to it go to the shell, and it exits as the
//! shell did.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};

/// How long the service has to answer, starting it when none runs.
const SERVICE_START: Duration = Duration::from_secs(30);
/// The shell the service started, which the signals sent here go to.
static SHELL: AtomicI32 = AtomicI32::new(0);
/// The signals that ask a session to stop or restart.
const FORWARDED: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGUSR1];

/// Whether this session is to ask the service for its shell: it was started with no link to it
/// (`runtime_collab::wire::shell::SHELL_FD_ENV`), on a computer whose service is the person's own,
/// and either a display manager started it (`host_desktop`, which starts the service when none
/// runs) or the service runs already. A session handed its window's connection to another desktop
/// (`WAYLAND_SOCKET`) holds a descriptor the service cannot pass on, and runs as it was started.
#[must_use]
pub fn asks_the_service(host_desktop: bool) -> bool {
    std::env::var_os(runtime_collab::wire::shell::SHELL_FD_ENV).is_none()
        && !tilo_api::daemon::system_daemon()
        && (host_desktop
            || (std::env::var_os("WAYLAND_SOCKET").is_none()
                && tilo_api::daemon::this_computer().is_ok_and(|place| place.alive())))
}

/// Have this computer's Tilo service start the person's shell with `args` (this session's own
/// arguments, but for the one-time preparation done here) and this session's environment, and
/// end as it does.
#[must_use]
pub fn through_the_service(args: &[String]) -> ExitCode {
    match start_and_wait(args) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("tilo-session: {why}");
            ExitCode::FAILURE
        }
    }
}

fn start_and_wait(args: &[String]) -> Result<ExitCode, String> {
    let place = tilo_api::daemon::this_computer()?;
    // A computer no person's certificate names yet founds its person here, as its first command
    // does (`devices#found`).
    place.wait_for_native(SERVICE_START)?;
    let socket = place.native();
    let stream = UnixStream::connect(&socket).map_err(|error| {
        format!(
            "Tilo's service is not answering at {}: {error}",
            socket.display()
        )
    })?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| format!("a connection to Tilo's service: {error}"))?,
    );
    let mut ask = |request: Value| -> Result<Value, String> {
        writeln!(&stream, "{request}")
            .and_then(|()| {
                let mut line = String::new();
                reader.read_line(&mut line).map(|_| line)
            })
            .map_err(|error| format!("Tilo's service stopped answering: {error}"))
            .and_then(|line| {
                serde_json::from_str::<Value>(&line)
                    .map_err(|error| format!("Tilo's service answered {line:?}: {error}"))
            })
            .and_then(|answer| match answer.get("error").and_then(Value::as_str) {
                Some(why) => Err(format!("Tilo's service refused: {why}")),
                None => Ok(answer),
            })
    };
    let env: Map<String, Value> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?.into()))
        })
        .collect();
    // The preparation `--firstboot` asks for is done here, once.
    let args: Vec<&String> = args.iter().filter(|arg| *arg != "--firstboot").collect();
    let started = ask(json!({"native": "shells#start", "args": args, "env": env}))?;
    let pid = started
        .get("pid")
        .and_then(Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| format!("Tilo's service started no shell: {started}"))?;
    SHELL.store(pid, Ordering::SeqCst);
    forward_signals()?;
    let ended = ask(json!({"native": "shells#wait", "pid": pid}))?;
    let code = match (
        ended.get("code").and_then(Value::as_i64),
        ended.get("signal").and_then(Value::as_i64),
    ) {
        (Some(code), _) => code,
        (None, Some(signal)) => signal.saturating_add(128),
        (None, None) => 1,
    };
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// Send each signal that asks a session to stop or restart on to the shell.
fn forward_signals() -> Result<(), String> {
    extern "C" fn forward(signal: libc::c_int) {
        let shell = SHELL.load(Ordering::SeqCst);
        if shell > 0 {
            // SAFETY: kill is async-signal-safe, and the pid is the shell's.
            unsafe { libc::kill(shell, signal) };
        }
    }
    for signal in FORWARDED {
        // SAFETY: an all-zero sigaction is a valid empty one, filled below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = forward as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // A wait for the shell's end goes on across a signal.
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: a valid action for a signal a process may catch.
        if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
            return Err(format!(
                "signals cannot reach the shell: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

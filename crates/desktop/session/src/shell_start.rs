//! A session started on a computer whose Tilo service runs as the person's own account never runs
//! as the person's shell itself: one a display manager starts (a distribution Tilo was added to),
//! one a development script starts beside the service it started first, and one started in a
//! window of another Tilo desktop. It asks that service to start the shell (`shells#start`), which
//! gives the shell the one-use secret it redeems for its link and, for a session in a window, the
//! connection it draws on, and stands in for it until it ends: the signals sent to it go to the
//! shell, and it exits as the shell did.

use std::io::{BufRead, BufReader, Write as _};
use std::os::fd::RawFd;
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
/// The connection a Wayland client draws its window on: what a session in a window of another
/// desktop is handed, and hands on when the service starts it.
const WAYLAND_SOCKET: &str = "WAYLAND_SOCKET";

/// What this session was started with: its link, the founding link of a computer whose person
/// this account makes, or nothing.
pub enum Launch {
    /// No link: the service did not start this session as a shell.
    None,
    /// The shell's link, which the session reads and writes its person's workspaces on.
    Shell(std::os::fd::OwnedFd),
    /// The sign-in's founding link: this account has no person yet, and this session may make one
    /// or join one, and asks again for its person's link once it has.
    Founding,
    /// The service refused this session's link, saying why: the person's services did not start,
    /// or the secret it was given was spent. The words are the ones the screen in front of the
    /// session shows when it has no workspace to open.
    Refused { why: String },
}

/// The link this session is the shell of this computer's service by: the one-use secret the
/// service gave it when it started it (`runtime_collab::wire::shell::SHELL_TOKEN_ENV`), redeemed
/// at once for the link with the one request that establishes one (`shells#link`), taken before
/// anything else of the session runs. The variable goes with it: it named a secret for this
/// process's own exec alone, so no program the session starts redeems it — a session started
/// beside this one asks the service for a shell of its own instead.
///
/// [`Launch::None`] in a process the service did not start as the shell, which is every other
/// one.
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "the variable names this process's own secret for its own exec; no other thread reads it yet and nothing else in the process uses it again"
)]
pub fn take_launch_link() -> Launch {
    let Ok(token) = std::env::var(runtime_collab::wire::shell::SHELL_TOKEN_ENV) else {
        return Launch::None;
    };
    // SAFETY: the session takes its link before it starts any thread of its own; the variable
    // named a secret for this process's own exec, and nothing reads the name again.
    unsafe { std::env::remove_var(runtime_collab::wire::shell::SHELL_TOKEN_ENV) };
    let Some(socket) = tilo_api::daemon::this_computer()
        .ok()
        .map(|place| place.native())
    else {
        eprintln!("tilo-session: this computer's service is not placed, so its shell has no link");
        return Launch::None;
    };
    match tilo_session_client::daemon_documents::link_with(&socket, Some(&token)) {
        Ok(tilo_session_client::daemon_documents::ShellLinkAnswer::Shell(link)) => {
            Launch::Shell(link)
        }
        Ok(tilo_session_client::daemon_documents::ShellLinkAnswer::Founding) => Launch::Founding,
        Err(error) => {
            eprintln!("tilo-session: the link this computer's service gave this session: {error}");
            Launch::Refused { why: error }
        }
    }
}

/// The person's link for the session a sign-in answered a founding link: asked of this computer's
/// service once the person exists. The daemon answers it once their services run, so the session
/// waits there rather than asking again. It says why when the service answers no link, and the
/// session then reads no workspace, as any session the daemon did not start.
///
/// # Errors
/// The service is not placed, answers no link, or answers with the failure that kept it from
/// starting the person's part.
pub fn founding_link() -> Result<std::os::fd::OwnedFd, String> {
    let socket = tilo_api::daemon::this_computer()
        .map(|place| place.native())
        .map_err(|error| format!("this computer's service is not placed: {error}"))?;
    match tilo_session_client::daemon_documents::link_with(&socket, None) {
        Ok(tilo_session_client::daemon_documents::ShellLinkAnswer::Shell(link)) => Ok(link),
        Ok(tilo_session_client::daemon_documents::ShellLinkAnswer::Founding) => {
            Err("this computer is still setting its person up".to_owned())
        }
        Err(error) => Err(error),
    }
}

/// The descriptor `name` names in this process's environment, when it names one: a descriptor
/// another program handed this process for its own use, never a number this one invented.
fn named_descriptor(name: &str) -> Option<RawFd> {
    let value = std::env::var(name).ok()?;
    tilo_failure::ignored!(
        "a variable that names no descriptor names neither a link nor a window this session holds",
        value.parse().ok()
    )
}

/// Whether this session is to ask the service for its shell: it holds no link of its own
/// (`handed`, from [`take_launch_link`]), on a computer whose service is the person's own, and
/// either a display manager started it (`host_desktop`, which starts the service when none runs)
/// or the service runs already. A session in a window of another desktop asks too: it sends the
/// connection it draws on with its start, and the service starts it with a link of its own.
#[must_use]
pub fn asks_the_service(host_desktop: bool, handed: bool) -> bool {
    if handed {
        return false;
    }
    // The daemon this session's environment names: a profile's own on Tilo OS too (a test's, a
    // development session's), which starts its shell as a person's own service does.
    let place = tilo_failure::ignored!(
        "a session whose service cannot be placed asks it only when a display manager started it, and that start says why it cannot",
        tilo_api::daemon::this_computer().ok()
    );
    match place {
        Some(place) => !place.system() && (host_desktop || place.alive()),
        None => host_desktop && !tilo_api::daemon::system_daemon(),
    }
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
    let stream = tilo_files::native::socket::connect(&socket).map_err(|error| {
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
    let env: Map<String, Value> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?.into()))
        })
        .collect();
    // The preparation `--firstboot` asks for is done here, once.
    let args: Vec<&String> = args.iter().filter(|arg| *arg != "--firstboot").collect();
    let start = json!({"native": "shells#start", "args": args, "env": env});
    // A session in a window of another desktop sends that window's connection with its start,
    // as a descriptor on this connection: the service starts the shell with it and with a link
    // of the shell's own.
    let started = if let Some(fd) = named_descriptor(WAYLAND_SOCKET) {
        let line = format!("{start}\n");
        // SAFETY: the descriptor is this process's window connection; sending a copy of it
        // leaves it this process's.
        let connection = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        runtime_portal::fd_transport::send_with_fd(&stream, line.as_bytes(), connection)
            .map_err(|error| format!("Tilo's service did not take the start: {error}"))?;
        answer(&mut reader)?
    } else {
        writeln!(&stream, "{start}")
            .map_err(|error| format!("Tilo's service stopped answering: {error}"))?;
        answer(&mut reader)?
    };
    let pid = started
        .get("pid")
        .and_then(Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| format!("Tilo's service started no shell: {started}"))?;
    SHELL.store(pid, Ordering::SeqCst);
    forward_signals()?;
    let wait = json!({"native": "shells#wait", "pid": pid});
    writeln!(&stream, "{wait}")
        .map_err(|error| format!("Tilo's service stopped answering: {error}"))?;
    let ended = answer(&mut reader)?;
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

/// The one-line answer Tilo's service wrote, refused when it says why.
fn answer(reader: &mut impl BufRead) -> Result<Value, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("Tilo's service stopped answering: {error}"))?;
    let answer = serde_json::from_str::<Value>(&line)
        .map_err(|error| format!("Tilo's service answered {line:?}: {error}"))?;
    match answer.get("error").and_then(Value::as_str) {
        Some(why) => Err(format!("Tilo's service refused: {why}")),
        None => Ok(answer),
    }
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

//! The command-line surface the shell invokes.

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tilo_projector::{DisplayResolutionLimit, DisplayStreamPolicy};

use crate::discovery::{DEFAULT_DISCOVERY_MS, discover};
use crate::error::ProjectorError;
use crate::session::cast::{parse_cast_protocol, run_cast_session};
use crate::session::external::{run_airplay_session, run_miracast_session};
use crate::session::paths::SessionPaths;
use crate::session::{SESSION_POLL_INTERVAL, SESSION_READY_TIMEOUT};

pub(crate) fn parse_stream_policy(args: &[String]) -> Result<DisplayStreamPolicy, ProjectorError> {
    let resolution_limit = match value_after(args, "--resolution-limit")
        .as_deref()
        .unwrap_or("native")
    {
        "native" => DisplayResolutionLimit::Native,
        "p2160" => DisplayResolutionLimit::P2160,
        "p1440" => DisplayResolutionLimit::P1440,
        "p1080" => DisplayResolutionLimit::P1080,
        "p720" => DisplayResolutionLimit::P720,
        value => {
            return Err(ProjectorError::Protocol(format!(
                "invalid display resolution limit {value}"
            )));
        }
    };
    let max_fps = value_after(args, "--max-fps")
        .as_deref()
        .unwrap_or("60")
        .parse::<u32>()
        .map_err(|_| ProjectorError::Protocol("invalid --max-fps".to_owned()))?;
    if !(24..=120).contains(&max_fps) {
        return Err(ProjectorError::Protocol(
            "--max-fps must be between 24 and 120".to_owned(),
        ));
    }
    Ok(DisplayStreamPolicy {
        adaptive: !args.iter().any(|arg| arg == "--fixed"),
        resolution_limit,
        max_fps,
    })
}

fn connect_command(args: &[String]) -> Result<(), ProjectorError> {
    let protocol = required(args, "--protocol")?;
    let session_command = match protocol.as_str() {
        "air-play" | "airplay" => "_airplay_session",
        "miracast" => "_miracast_session",
        _ => {
            parse_cast_protocol(&protocol)?;
            "_session"
        }
    };
    let paths = SessionPaths::resolve()?;
    paths.clear_markers();
    let executable = std::env::current_exe().map_err(ProjectorError::io(
        "cannot resolve tilo-projector executable",
    ))?;
    let mut child_args = vec![session_command.to_owned()];
    child_args.extend_from_slice(args.get(1..).unwrap_or(&[]));
    let mut child = Command::new(executable)
        .args(child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ProjectorError::io("cannot start projection session"))?;
    let deadline = Instant::now()
        .checked_add(SESSION_READY_TIMEOUT)
        .ok_or_else(|| {
            ProjectorError::Unavailable(
                "system clock cannot represent the session-ready deadline".to_owned(),
            )
        })?;
    while Instant::now() < deadline {
        if paths.ready.exists() {
            return Ok(());
        }
        // The session reports its failure across a process boundary, as the
        // text the user will see; its category does not survive the file.
        if let Ok(error) = fs::read_to_string(&paths.error) {
            return Err(ProjectorError::Receiver(error));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(ProjectorError::io("cannot inspect projection session"))?
        {
            return Err(ProjectorError::Receiver(format!(
                "projection session exited before it was ready ({status})"
            )));
        }
        std::thread::sleep(SESSION_POLL_INTERVAL);
    }
    // Give the worker a chance to terminate its transport child as well. This
    // matters for AirPlay, whose sender is a separate statically-linked helper.
    let _ = fs::write(&paths.stop, b"stop");
    if let Some(cleanup_deadline) = Instant::now().checked_add(Duration::from_secs(2)) {
        while Instant::now() < cleanup_deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(SESSION_POLL_INTERVAL);
        }
    }
    let _ = child.kill();
    Err(ProjectorError::Timeout(
        "projection session did not become ready in time",
    ))
}

fn disconnect_command() -> Result<(), ProjectorError> {
    let paths = SessionPaths::resolve()?;
    if !paths.pid.exists() {
        paths.clear_markers();
        return Ok(());
    }
    fs::write(&paths.stop, b"stop")
        .map_err(ProjectorError::io("cannot stop projection session"))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or_else(|| {
            ProjectorError::Unavailable(
                "system clock cannot represent the disconnect deadline".to_owned(),
            )
        })?;
    while paths.pid.exists() && Instant::now() < deadline {
        std::thread::sleep(SESSION_POLL_INTERVAL);
    }
    if paths.pid.exists() {
        return Err(ProjectorError::Timeout(
            "projection session did not stop cleanly",
        ));
    }
    Ok(())
}

pub(crate) fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| match pair {
        [name, value] if name == flag => Some(value.clone()),
        _ => None,
    })
}

/// The value after `flag`, which the command cannot run without.
pub(crate) fn required(args: &[String], flag: &str) -> Result<String, ProjectorError> {
    value_after(args, flag).ok_or_else(|| ProjectorError::missing(flag))
}

/// Runs a detached session child. On failure it leaves the message in the
/// `error` marker for the waiting `connect` and withdraws its pid, so neither
/// `connect` nor a later `disconnect` mistakes it for a live session.
fn session_main(
    session: fn(&[String]) -> Result<(), ProjectorError>,
    args: &[String],
) -> Result<(), ProjectorError> {
    session(args).inspect_err(|error| {
        if let Ok(paths) = SessionPaths::resolve() {
            let _ = fs::write(&paths.error, error.to_string());
            let _ = fs::remove_file(&paths.pid);
        }
    })
}

pub(crate) fn run(args: &[String]) -> Result<(), ProjectorError> {
    match args.first().map(String::as_str) {
        Some("discover") => {
            let timeout_ms = value_after(args, "--timeout-ms")
                .map(|raw| {
                    raw.parse::<u64>()
                        .map_err(|_| ProjectorError::Protocol("invalid --timeout-ms".to_owned()))
                })
                .transpose()?
                .unwrap_or(DEFAULT_DISCOVERY_MS);
            let devices = discover(Duration::from_millis(timeout_ms))?;
            println!(
                "{}",
                serde_json::to_string(&devices).map_err(|error| ProjectorError::Unavailable(
                    format!("cannot encode discovery result: {error}")
                ))?
            );
            Ok(())
        }
        Some("connect") => connect_command(args),
        Some("disconnect") => disconnect_command(),
        Some("_session") => session_main(run_cast_session, args),
        Some("_airplay_session") => session_main(run_airplay_session, args),
        Some("_miracast_session") => session_main(run_miracast_session, args),
        _ => Err(ProjectorError::Protocol(
            "usage: tilo-projector discover --json [--timeout-ms N] | connect ... | disconnect ..."
                .to_owned(),
        )),
    }
}

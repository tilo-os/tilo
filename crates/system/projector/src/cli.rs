//! The command line: `serve` runs the broker; `discover` searches once in
//! process; `connect` and `disconnect` are clients of a running broker's
//! control socket, for scripts and diagnosis.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use tilo_projector::control::{self, ProjectorEvent, ProjectorRequest};
use tilo_projector::{
    DisplayResolutionLimit, DisplayStreamPolicy, ProjectionDevice, ProjectionMode,
    ProjectionOptions, ProjectionProtocol,
};

use crate::discovery::{DEFAULT_DISCOVERY_MS, discover};
use crate::error::ProjectorError;
use crate::service::{self, Service};
use crate::session::Helpers;
use crate::session::paths::SessionPaths;

const USAGE: &str = "usage: tilo-projector serve [--socket PATH] \
     | discover [--json] [--timeout-ms N] \
     | connect --protocol P --address A --port N [--device NAME] [--resolution-limit L] [--max-fps N] [--fixed] [--no-audio] [--socket PATH] \
     | disconnect [--socket PATH]";

fn parse_stream_policy(args: &[String]) -> Result<DisplayStreamPolicy, ProjectorError> {
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

fn parse_protocol(value: &str) -> Result<ProjectionProtocol, ProjectorError> {
    match value {
        "air-play" | "airplay" => Ok(ProjectionProtocol::AirPlay),
        "miracast" => Ok(ProjectionProtocol::Miracast),
        "f-cast" | "fcast" => Ok(ProjectionProtocol::FCast),
        "chromecast" => Ok(ProjectionProtocol::Chromecast),
        other => Err(ProjectorError::Protocol(format!(
            "unknown projection protocol {other}"
        ))),
    }
}

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| match pair {
        [name, value] if name == flag => Some(value.clone()),
        _ => None,
    })
}

/// The value after `flag`, which the command cannot run without.
fn required(args: &[String], flag: &str) -> Result<String, ProjectorError> {
    value_after(args, flag).ok_or_else(|| ProjectorError::missing(flag))
}

fn serve(args: &[String]) -> Result<(), ProjectorError> {
    let paths = SessionPaths::resolve()?;
    let listener = service::listen(&paths, value_after(args, "--socket").map(PathBuf::from))?;
    Service::new(paths, Helpers::from_env()).run(&listener)
}

/// A connection to the running broker.
fn client(args: &[String]) -> Result<UnixStream, ProjectorError> {
    let socket = value_after(args, "--socket")
        .map(PathBuf::from)
        .or_else(control::control_socket)
        .ok_or_else(|| ProjectorError::Unavailable("XDG_RUNTIME_DIR is unavailable".to_owned()))?;
    UnixStream::connect(&socket).map_err(ProjectorError::io(format!(
        "cannot reach the projector at {}",
        socket.display()
    )))
}

fn next_event(stream: &mut UnixStream) -> Result<ProjectorEvent, ProjectorError> {
    control::read_event(stream)
        .map_err(|error| {
            ProjectorError::Unavailable(format!("projector connection failed: {error}"))
        })?
        .ok_or_else(|| {
            ProjectorError::Unavailable("the projector closed the connection".to_owned())
        })
}

fn send(stream: &mut UnixStream, request: &ProjectorRequest) -> Result<(), ProjectorError> {
    control::write_request(stream, request).map_err(|error| {
        ProjectorError::Unavailable(format!("projector connection failed: {error}"))
    })
}

fn print_json(value: &impl serde::Serialize) -> Result<(), ProjectorError> {
    let line = serde_json::to_string(value)
        .map_err(|error| ProjectorError::Unavailable(format!("cannot encode output: {error}")))?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .map_err(ProjectorError::io("cannot write output"))
}

/// Projects until the session ends, printing each event as a JSON line. The
/// session belongs to this connection: interrupting the command stops it,
/// unless another client (the shell) is connected too.
fn connect_command(args: &[String]) -> Result<(), ProjectorError> {
    let protocol = parse_protocol(&required(args, "--protocol")?)?;
    let address = required(args, "--address")?;
    let port = required(args, "--port")?
        .parse::<u16>()
        .map_err(|_| ProjectorError::Protocol("invalid --port".to_owned()))?;
    let name = value_after(args, "--device").unwrap_or_else(|| "Wireless display".to_owned());
    let device = ProjectionDevice {
        id: format!("{}:{address}", protocol.label()),
        name,
        protocol,
        detail: address.clone(),
        modes: vec![ProjectionMode::Mirror],
        addresses: vec![address],
        port,
    };
    let options = ProjectionOptions {
        mode: ProjectionMode::Mirror,
        audio: !args.iter().any(|arg| arg == "--no-audio"),
        stream_policy: parse_stream_policy(args)?,
        credential: None,
    };
    let mut stream = client(args)?;
    send(
        &mut stream,
        &ProjectorRequest::Connect {
            device: device.clone(),
            options,
            credential: None,
        },
    )?;
    loop {
        let event = next_event(&mut stream)?;
        print_json(&event)?;
        if let ProjectorEvent::Ended { device_id, error } = event
            && device_id == device.id
        {
            return error.map_or(Ok(()), |error| Err(ProjectorError::Receiver(error)));
        }
    }
}

fn disconnect_command(args: &[String]) -> Result<(), ProjectorError> {
    let mut stream = client(args)?;
    send(&mut stream, &ProjectorRequest::Disconnect)?;
    loop {
        match next_event(&mut stream)? {
            ProjectorEvent::Idle => return Ok(()),
            ProjectorEvent::Failed(error) => return Err(ProjectorError::Receiver(error)),
            _ => {}
        }
    }
}

pub(crate) fn run(args: &[String]) -> Result<(), ProjectorError> {
    match args.first().map(String::as_str) {
        Some("serve") => serve(args),
        Some("discover") => {
            let timeout_ms = value_after(args, "--timeout-ms")
                .map(|raw| {
                    raw.parse::<u64>()
                        .map_err(|_| ProjectorError::Protocol("invalid --timeout-ms".to_owned()))
                })
                .transpose()?
                .unwrap_or(DEFAULT_DISCOVERY_MS);
            print_json(&discover(Duration::from_millis(timeout_ms))?)
        }
        Some("connect") => connect_command(args),
        Some("disconnect") => disconnect_command(args),
        _ => Err(ProjectorError::Protocol(USAGE.to_owned())),
    }
}

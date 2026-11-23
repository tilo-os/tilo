//! A stand-in for the wireless display service, on its control socket.
//!
//! The display sheet speaks to the projector service over one control socket
//! (`tilo_projector::control`): it asks for receivers, asks to project to one
//! and asks to stop. A headless session a script drives has no projector
//! service beside it, so this answers those three requests with one receiver
//! that is found, connects and stops — enough for a person's flow through the
//! sheet to be walked without a projector or a real receiver.
//!
//! It says nothing about how a real broker projects: no frames cross, and the
//! receiver has no PIN. It is the sheet's own path that is under test.
//!
//! `projector-stand-in [--socket PATH]` listens where the session looks for the
//! service (`$TILO_PROJECTOR_RUNTIME_DIR/control.sock`, else the runtime
//! directory's `tilo-projector/control.sock`), prints `ready <socket>` and
//! serves until interrupted.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use tilo_files::native::socket;

use tilo_projector::control::{self, ConnectStage, ProjectorEvent, ProjectorRequest};
use tilo_projector::{ProjectionDevice, ProjectionMode, ProjectionProtocol};

fn main() -> std::io::Result<()> {
    let mut socket: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {other}; use --socket PATH"
                )));
            }
        }
    }
    let Some(socket) = socket.or_else(control::control_socket) else {
        return Err(std::io::Error::other(
            "no socket: pass --socket, or set TILO_PROJECTOR_RUNTIME_DIR",
        ));
    };
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener = socket::bind(&socket)?;
    println!("ready {}", socket.display());
    std::io::stdout().flush()?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(error) = serve(stream) {
                        eprintln!("projector-stand-in: {error}");
                    }
                });
            }
            Err(error) => eprintln!("projector-stand-in: {error}"),
        }
    }
    Ok(())
}

/// The receiver the sheet finds: a living-room television that mirrors.
fn receiver() -> ProjectionDevice {
    ProjectionDevice {
        id: "cast:stand-in:living-room".to_owned(),
        name: "Living room".to_owned(),
        protocol: ProjectionProtocol::Chromecast,
        detail: "Local network".to_owned(),
        modes: vec![ProjectionMode::Mirror],
        addresses: vec!["192.0.2.7".to_owned()],
        port: 8009,
    }
}

/// Answer the session's requests on `stream` until it closes: looking finds the
/// receiver, connecting reports it starting and then connected, and stopping
/// says nothing runs.
fn serve(mut stream: UnixStream) -> std::io::Result<()> {
    let device = receiver();
    while let Some(request) = control::read_request(&mut stream)
        .map_err(|error| std::io::Error::other(error.to_string()))?
    {
        let events: Vec<ProjectorEvent> = match request {
            ProjectorRequest::Discover { .. } => {
                vec![ProjectorEvent::Devices(vec![device.clone()])]
            }
            ProjectorRequest::Connect { device, .. } => vec![
                ProjectorEvent::Connecting {
                    device_id: device.id.clone(),
                    stage: ConnectStage::Starting,
                },
                ProjectorEvent::Connected {
                    device_id: device.id,
                },
            ],
            ProjectorRequest::Disconnect => vec![ProjectorEvent::Idle],
        };
        for event in events {
            control::write_event(&mut stream, &event)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
    }
    Ok(())
}

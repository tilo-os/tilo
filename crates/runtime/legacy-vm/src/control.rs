//! The supervisor's control socket: `tilo-legacy` commands ask it to wake
//! the VM, suspend it, or report.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wire_frame::Framing;

pub const FRAMING: Framing = Framing::little_endian(64 * 1024).reject_empty();

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControlRequest {
    /// Wake the VM and make sure the granted directories, plus `extra`
    /// for as long as the VM stays awake, are shared.
    Ensure {
        extra: Vec<PathBuf>,
    },
    Suspend,
    Status,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControlReply {
    Ready { cid: u32, waypipe_port: u32 },
    Suspending,
    Status(StatusReport),
    Error(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub phase: String,
    pub cid: Option<u32>,
    pub shares: Vec<String>,
    pub memory_mib: Option<u64>,
    pub running: Option<u32>,
}

/// Send one request and wait for the reply. `None` when no supervisor
/// listens.
pub fn ask(socket: &Path, request: &ControlRequest) -> io::Result<Option<ControlReply>> {
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    FRAMING
        .write_value(&mut stream, request)
        .map_err(io::Error::other)?;
    FRAMING.read_value(&mut stream).map_err(io::Error::other)
}

/// [`ask`], retrying while a just-spawned supervisor binds its socket.
pub fn ask_within(
    socket: &Path,
    request: &ControlRequest,
    patience: Duration,
) -> io::Result<Option<ControlReply>> {
    let started = Instant::now();
    loop {
        match ask(socket, request)? {
            Some(reply) => return Ok(Some(reply)),
            None if started.elapsed() >= patience => return Ok(None),
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

//! How the host reaches a guest's vsock ports.
//!
//! The guest always speaks `AF_VSOCK`. On the host there are two ways in:
//!
//! - **Kernel**: the domain has a `vhost-vsock` device and the host connects
//!   with `AF_VSOCK` to the guest's context id. Needs `/dev/vhost-vsock`.
//! - **Hybrid**: QEMU's `vhost-user-vsock` device is served by the
//!   unprivileged `vhost-device-vsock` daemon, which maps the guest's vsock
//!   onto Unix sockets: the host connects to one socket and asks for a port
//!   (`CONNECT <port>\n` → `OK <port>\n`), and a guest connecting to the
//!   host's port `P` reaches the Unix socket `<socket>_P`. For hosts whose
//!   kernel has no `vhost_vsock` (nested and sandboxed hosts).

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Duplex;
#[cfg(target_os = "linux")]
use crate::vsock::VsockStream;

/// The context id a hybrid-vsock guest is given: the namespace is the
/// daemon's own, so it never collides with another VM's.
pub const HYBRID_GUEST_CID: u32 = 3;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentLink {
    Kernel { cid: u32 },
    Hybrid { socket: PathBuf },
}

impl AgentLink {
    /// A stream to the guest's `port`.
    pub fn connect(&self, port: u32) -> io::Result<AgentStream> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel { cid } => Ok(AgentStream::Kernel(VsockStream::connect(*cid, port)?)),
            #[cfg(not(target_os = "linux"))]
            Self::Kernel { .. } => Err(io::Error::from(io::ErrorKind::Unsupported)),
            Self::Hybrid { socket } => Ok(AgentStream::Hybrid(hybrid_connect(socket, port)?)),
        }
    }

    /// Where a hybrid host listens for the guest's connections to `port`.
    #[must_use]
    pub fn hybrid_listener(socket: &Path, port: u32) -> PathBuf {
        let mut path = socket.as_os_str().to_owned();
        path.push(format!("_{port}"));
        PathBuf::from(path)
    }
}

fn hybrid_connect(socket: &Path, port: u32) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(format!("CONNECT {port}\n").as_bytes())?;
    // Read the reply byte by byte: anything after the newline is the
    // guest's own data and must stay in the stream.
    let mut reply = Vec::new();
    while reply.len() < 64 {
        let mut byte = [0_u8; 1];
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
        }
        let [byte] = byte;
        if byte == b'\n' {
            break;
        }
        reply.push(byte);
    }
    stream.set_read_timeout(None)?;
    if reply.starts_with(b"OK ") {
        Ok(stream)
    } else {
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            String::from_utf8_lossy(&reply).into_owned(),
        ))
    }
}

/// One connection to a guest port, over either link.
#[derive(Debug)]
pub enum AgentStream {
    #[cfg(target_os = "linux")]
    Kernel(VsockStream),
    Hybrid(UnixStream),
}

impl AgentStream {
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => stream.set_read_timeout(timeout),
            Self::Hybrid(stream) => stream.set_read_timeout(timeout),
        }
    }
}

impl Read for AgentStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => stream.read(buf),
            Self::Hybrid(stream) => stream.read(buf),
        }
    }
}

impl Write for AgentStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => stream.write(buf),
            Self::Hybrid(stream) => stream.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Duplex for AgentStream {
    fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => Ok(Self::Kernel(Duplex::try_clone(stream)?)),
            Self::Hybrid(stream) => Ok(Self::Hybrid(UnixStream::try_clone(stream)?)),
        }
    }
    fn shutdown_write(&self) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => stream.shutdown_write(),
            Self::Hybrid(stream) => Duplex::shutdown_write(stream),
        }
    }
    fn shutdown_both(&self) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Kernel(stream) => stream.shutdown_both(),
            Self::Hybrid(stream) => Duplex::shutdown_both(stream),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;
    use crate::{HostMessage, recv, send};

    #[test]
    fn a_hybrid_link_asks_for_the_port_then_carries_frames() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("vsock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            let mut byte = [0_u8; 1];
            while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            assert_eq!(line, b"CONNECT 7110");
            stream.write_all(b"OK 1073741824\n").unwrap();
            recv::<_, HostMessage>(&mut stream).unwrap()
        });
        let link = AgentLink::Hybrid {
            socket: socket.clone(),
        };
        let mut stream = link.connect(7110).unwrap();
        send(&mut stream, &HostMessage::Health).unwrap();
        assert_eq!(daemon.join().unwrap(), Some(HostMessage::Health));
        assert_eq!(
            AgentLink::hybrid_listener(&socket, 7111),
            dir.path().join("vsock.sock_7111")
        );
    }
}

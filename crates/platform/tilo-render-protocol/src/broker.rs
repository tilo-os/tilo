//! How an app gets its channel to its room's render service.
//!
//! The launcher connects the compositor's render broker
//! (`$XDG_RUNTIME_DIR/tilo/services/render.sock`) on the app's behalf and
//! writes [`handshake`] — the app and its room — before the sandboxed app
//! holds the connection, then hands it in as `TILO_SVC_RENDER_FD`. The app
//! asks on it for a channel ([`request_channel`]): one byte out, one byte
//! and the channel's descriptor back. It asks again when a channel breaks
//! (the service restarted), and once per window, so every window renders on
//! a channel of its own.
//!
//! The broker, not the app, chose the room: the app can only ever reach the
//! service of the room it was launched into.

use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::BorrowedFd;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};

use crate::channel::Channel;

/// The service name: the broker's socket is `services/render.sock`, and the
/// app finds its connection in `TILO_SVC_RENDER_FD`.
pub const SERVICE: &str = "render";

/// The app's connection to the broker, as the launcher hands it in.
pub const FD_ENV: &str = "TILO_SVC_RENDER_FD";

/// For a process outside the sandbox (a developer's run, a measurement): the
/// broker's socket to connect itself, writing its own handshake.
pub const SOCKET_ENV: &str = "TILO_RENDER_SOCKET";

/// Version of the handshake line.
pub const PROTOCOL: u16 = 1;

/// The request byte: one channel, please.
const REQUEST: u8 = b'C';
/// The answer byte with a channel.
const GRANTED: u8 = b'K';
/// The answer byte without one (no service for this room right now).
const REFUSED: u8 = b'N';

/// Longest handshake line the broker reads.
pub const MAX_HANDSHAKE: usize = 4096;

/// The line that names an app and its room on a new broker connection.
///
/// # Errors
///
/// When the names cannot be encoded.
pub fn handshake(app_id: &str, workspace_id: Option<&str>) -> io::Result<Vec<u8>> {
    #[derive(serde::Serialize)]
    struct Line<'a> {
        protocol: u16,
        app_id: &'a str,
        workspace_id: Option<&'a str>,
    }
    let mut line = postcard::to_allocvec(&Line {
        protocol: PROTOCOL,
        app_id,
        workspace_id,
    })
    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if line.len() > MAX_HANDSHAKE.saturating_sub(4) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut framed = u32::try_from(line.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_le_bytes()
        .to_vec();
    framed.append(&mut line);
    Ok(framed)
}

/// A handshake as the broker reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Handshake {
    pub protocol: u16,
    pub app_id: String,
    pub workspace_id: Option<String>,
}

/// Read the handshake from a new broker connection.
///
/// # Errors
///
/// When the connection ends early or the line is not a handshake.
pub fn read_handshake(stream: &mut impl io::Read) -> io::Result<Handshake> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_HANDSHAKE {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut line = vec![0_u8; length];
    stream.read_exact(&mut line)?;
    let handshake: Handshake =
        postcard::from_bytes(&line).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if handshake.protocol != PROTOCOL || handshake.app_id.is_empty() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(handshake)
}

/// Write `handshake` to a connection.
///
/// # Errors
///
/// When the write fails.
pub fn write_handshake(
    stream: &mut impl Write,
    app_id: &str,
    workspace_id: Option<&str>,
) -> io::Result<()> {
    stream.write_all(&handshake(app_id, workspace_id)?)?;
    stream.flush()
}

/// Ask the broker on `broker` for a channel. Blocks until it answers.
///
/// # Errors
///
/// When the broker is gone, refuses, or answers with something else.
pub fn request_channel(broker: BorrowedFd<'_>) -> io::Result<Channel> {
    let sent = rustix::net::send(broker, &[REQUEST], SendFlags::NOSIGNAL)?;
    if sent != 1 {
        return Err(io::Error::from(io::ErrorKind::WriteZero));
    }
    let mut answer = [0_u8; 1];
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = loop {
        match rustix::net::recvmsg(
            broker,
            &mut [IoSliceMut::new(&mut answer)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Err(rustix::io::Errno::INTR) => {}
            other => break other?,
        }
    };
    let mut fds = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    match (received.bytes, answer, fds.pop()) {
        (1, [GRANTED], Some(fd)) => Channel::from_socket(fd),
        (1, [REFUSED], _) => Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "no render service for this room",
        )),
        (0, _, _) => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
        _ => Err(io::Error::from(io::ErrorKind::InvalidData)),
    }
}

/// Wait for the next request on a broker connection. `Ok(false)` at end of
/// stream.
///
/// # Errors
///
/// When the read fails or the byte is not a request.
pub fn read_request(broker: BorrowedFd<'_>) -> io::Result<bool> {
    let mut byte = [0_u8; 1];
    let read = loop {
        match rustix::io::read(broker, &mut byte) {
            Err(rustix::io::Errno::INTR) => {}
            other => break other?,
        }
    };
    match (read, byte) {
        (0, _) => Ok(false),
        (1, [REQUEST]) => Ok(true),
        _ => Err(io::Error::from(io::ErrorKind::InvalidData)),
    }
}

/// Answer a request with `channel`, or refuse it.
///
/// # Errors
///
/// When the send fails.
pub fn answer(broker: BorrowedFd<'_>, channel: Option<&Channel>) -> io::Result<()> {
    let fds: Vec<BorrowedFd<'_>> = channel.map(Channel::as_fd).into_iter().collect();
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    let byte = if fds.is_empty() {
        REFUSED
    } else {
        if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        GRANTED
    };
    rustix::net::sendmsg(
        broker,
        &[IoSlice::new(&[byte])],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    Ok(())
}

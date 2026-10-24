use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use wire_frame::{FrameError, Framing};

use crate::{BrokerSigningIdentity, NotAvailable, wire};
use std::sync::Arc;

pub const BROKER_ENV_VAR: &str = "TILO_IDENTITY_BROKER_FD";
pub const BROKER_CHILD_FD: RawFd = 15;
pub const OP_METADATA: u8 = 1;
pub const OP_SIGN: u8 = 2;
static BROKER_FD_CLAIMED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize)]
pub struct PublicMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_photo: Option<crate::ProfilePhoto>,
    pub display_name: String,
    pub avatar_color: [u8; 3],
    pub group_public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub share_index: u16,
    pub single_device: bool,
}

/// A status or operation byte, then one big-endian length-prefixed payload.
pub const FRAMING: Framing = Framing::big_endian(4 * 1024 * 1024);

pub fn write_frame(stream: &mut UnixStream, status: u8, payload: &[u8]) -> io::Result<()> {
    let frame = FRAMING.encode(payload)?;
    stream.write_all(&[status])?;
    stream.write_all(&frame)
}

pub fn read_frame(stream: &mut UnixStream) -> io::Result<(u8, Vec<u8>)> {
    let mut status = [0_u8; 1];
    stream.read_exact(&mut status)?;
    let payload = FRAMING.read(stream)?.ok_or(FrameError::Truncated)?;
    Ok((status[0], payload))
}

pub fn connect_from_env() -> Result<Arc<BrokerSigningIdentity>, NotAvailable> {
    let raw_value = std::env::var(BROKER_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
    let raw = raw_value.parse::<RawFd>().map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity broker fd",
        ))
    })?;
    if raw < 3 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity broker fd",
        )));
    }
    // SAFETY: this checks the descriptor without taking ownership.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags < 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::last_os_error()));
    }
    // Atomically claim the inherited authority before constructing an owner.
    // The environment is process-global and may be read concurrently, so it is
    // intentionally left untouched. A second load() fails before it can create
    // another UnixStream owner for the same raw descriptor.
    BROKER_FD_CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "identity broker fd has already been claimed by this process",
            ))
        })?;
    // SAFETY: Tilo launcher transfers sole ownership of this inherited fd to
    // the app. load() consumes it once, matching runtime-portal's contract.
    let mut stream = unsafe { UnixStream::from_raw_fd(raw) };
    // SAFETY: the stream owns raw; later execs must not inherit its authority.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::last_os_error()));
    }
    write_frame(&mut stream, OP_METADATA, &[]).map_err(NotAvailable::ConnectionFailed)?;
    let (status, payload) = read_frame(&mut stream).map_err(NotAvailable::ConnectionFailed)?;
    if status != 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity broker refused metadata",
        )));
    }
    let metadata: PublicMetadata = serde_json::from_slice(&payload).map_err(|e| {
        NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    Ok(Arc::new(BrokerSigningIdentity::from_legacy_stream(
        stream, metadata,
    )))
}

pub(crate) fn exchange(
    stream: &mut UnixStream,
    request: &wire::Request,
) -> Result<wire::Response, NotAvailable> {
    let message = match request {
        wire::Request::Sign { message, .. } => message.clone(),
        wire::Request::CollaborationJoin { workspace_id } => {
            crate::collaboration_join_signing_bytes(workspace_id)
        }
        _ => {
            return Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "certified collaboration requires the workspace identity service",
            )));
        }
    };
    write_frame(stream, OP_SIGN, &message)?;
    let (status, payload) = read_frame(stream)?;
    if status != 0 {
        return Err(crate::client::permission_denied(
            String::from_utf8_lossy(&payload).into_owned(),
        ));
    }
    Ok(wire::Response::Signature(payload))
}

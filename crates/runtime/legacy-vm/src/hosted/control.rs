//! The hosted-VM service's socket: ush asks it for a workspace's VM, and
//! operators ask it what runs.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use legacy_agent_protocol::link::AgentLink;
use serde::{Deserialize, Serialize};
use wire_frame::Framing;

use super::meter::{SizeUsage, UsageRecord};
use super::size::VmSize;

pub const FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024).reject_empty();

/// Which VM, whose, and how large.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmRequest {
    pub workspace: String,
    pub environment: String,
    pub account: String,
    pub size: VmSize,
}

/// A directory the VM sees at the same path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShareRequest {
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Wake the VM (starting it on first use) with `shares` mounted.
    Ensure {
        vm: VmRequest,
        shares: Vec<ShareRequest>,
    },
    /// Snapshot the VM now.
    Suspend {
        workspace: String,
        environment: String,
    },
    /// Shut the VM down; its disk stays.
    Stop {
        workspace: String,
        environment: String,
    },
    Status,
    /// The account's records that ended in `[from_ms, to_ms)`, and their sum
    /// per size.
    Usage {
        account: String,
        from_ms: u64,
        to_ms: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmStatus {
    pub vm: String,
    pub account: String,
    pub size: VmSize,
    pub phase: String,
    pub last_wake_ms: Option<u64>,
    pub last_wake_restored: Option<bool>,
    pub snapshot_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Ready {
        link: AgentLink,
        /// What waking took, when this request woke it.
        woke_ms: Option<u64>,
    },
    /// The account's plan does not allow it: why, in words for whoever
    /// ran it, and what would let it through.
    Refused(tilo_actions::remote::host_account::Refusal),
    Done,
    Status(Vec<VmStatus>),
    Usage {
        records: Vec<UsageRecord>,
        by_size: Vec<SizeUsage>,
    },
    Error(String),
}

/// One request, one reply. `None` when no service listens at `socket`.
pub fn ask(socket: &Path, request: &Request, patience: Duration) -> io::Result<Option<Reply>> {
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
    stream.set_read_timeout(Some(patience))?;
    FRAMING
        .write_value(&mut stream, request)
        .map_err(io::Error::other)?;
    FRAMING.read_value(&mut stream).map_err(io::Error::other)
}

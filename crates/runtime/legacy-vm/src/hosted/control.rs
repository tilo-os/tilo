//! The hosted-VM service's socket: ush asks it for a workspace's VM, and
//! operators ask it what runs. A placer ([`super::placement`]) speaks the same
//! protocol: it reads each host's [`Request::Status`] and answers
//! [`Request::Place`] with the host a workspace's VM runs on.

use std::io;
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
    /// Which host runs this VM (a placer's question): the one holding it,
    /// else the first with room. New variants go last: the encoding names
    /// a variant by its place.
    Place {
        vm: VmRequest,
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

/// What a host's VMs take of its memory budget, by their sizes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostLoad {
    /// The most its awake VMs may take together; `None` when only its slots
    /// bound them.
    pub budget_mib: Option<u64>,
    /// What its awake VMs take.
    pub awake_mib: u64,
    /// What wakes waiting for room on it ask for.
    pub waiting_mib: u64,
}

impl HostLoad {
    /// What one more VM may take here now.
    #[must_use]
    pub fn free_mib(&self) -> u64 {
        self.budget_mib
            .map_or(u64::MAX, |budget| budget.saturating_sub(self.awake_mib))
    }
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
    Status {
        vms: Vec<VmStatus>,
        load: HostLoad,
    },
    Usage {
        #[serde(with = "usage_on_the_wire")]
        records: Vec<UsageRecord>,
        by_size: Vec<SizeUsage>,
    },
    Error(String),
    /// Not now, and not refused: the VM waits for room (a machine being
    /// started, or VMs going idle), in words for whoever ran it. Ask again.
    Busy(String),
    /// The host the VM runs on (a placer's answer).
    Placed {
        host: String,
    },
}

/// The socket's frames are postcard, which encodes neither the ledger's
/// flattened identity nor its skipped empty fields: on the socket a record
/// travels field by field, every field always present.
mod usage_on_the_wire {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::super::meter::{RunEnd, UsageKind, UsageRecord, VmIdentity};

    #[derive(Serialize, Deserialize)]
    struct Wire {
        kind: UsageKind,
        identity: VmIdentity,
        start_ms: u64,
        end_ms: u64,
        millis: u64,
        seconds: u64,
        ended: Option<RunEnd>,
        snapshot_bytes: Option<u64>,
    }

    pub fn serialize<S: Serializer>(records: &[UsageRecord], to: S) -> Result<S::Ok, S::Error> {
        to.collect_seq(records.iter().map(|record| Wire {
            kind: record.kind,
            identity: record.identity.clone(),
            start_ms: record.start_ms,
            end_ms: record.end_ms,
            millis: record.millis,
            seconds: record.seconds,
            ended: record.ended,
            snapshot_bytes: record.snapshot_bytes,
        }))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<UsageRecord>, D::Error> {
        Ok(Vec::<Wire>::deserialize(from)?
            .into_iter()
            .map(|wire| UsageRecord {
                kind: wire.kind,
                identity: wire.identity,
                start_ms: wire.start_ms,
                end_ms: wire.end_ms,
                millis: wire.millis,
                seconds: wire.seconds,
                ended: wire.ended,
                snapshot_bytes: wire.snapshot_bytes,
            })
            .collect())
    }
}

/// One request, one reply. `None` when no service listens at `socket`.
pub fn ask(socket: &Path, request: &Request, patience: Duration) -> io::Result<Option<Reply>> {
    let mut stream = match tilo_files::native::socket::connect(socket) {
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

#[cfg(test)]
mod tests {
    use super::super::meter::{RunEnd, UsageKind, VmIdentity};
    use super::*;

    fn over_the_socket(reply: &Reply) -> Reply {
        let mut frame = Vec::new();
        FRAMING.write_value(&mut frame, reply).unwrap();
        FRAMING.read_value(&mut frame.as_slice()).unwrap().unwrap()
    }

    #[test]
    fn usage_crosses_the_socket_whole() {
        let identity = VmIdentity {
            vm: "ws/default".into(),
            workspace: "ws".into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size: VmSize::Small,
        };
        let reply = Reply::Usage {
            records: vec![
                UsageRecord {
                    kind: UsageKind::Running,
                    identity: identity.clone(),
                    start_ms: 1_000,
                    end_ms: 4_500,
                    millis: 3_500,
                    seconds: 3,
                    ended: Some(RunEnd::Suspended),
                    snapshot_bytes: None,
                },
                UsageRecord {
                    kind: UsageKind::Snapshot,
                    identity,
                    start_ms: 4_500,
                    end_ms: 9_000,
                    millis: 4_500,
                    seconds: 4,
                    ended: None,
                    snapshot_bytes: Some(1 << 20),
                },
            ],
            by_size: vec![SizeUsage {
                size: VmSize::Small,
                running_seconds: 3,
                snapshot_byte_seconds: 4 << 20,
            }],
        };
        assert_eq!(over_the_socket(&reply), reply);
    }
}

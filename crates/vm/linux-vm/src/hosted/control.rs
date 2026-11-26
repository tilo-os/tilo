//! The hosted-VM service's socket: ush asks it for a workspace's VM, and operators
//! ask it what runs.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use linux_vm_protocol::link::AgentLink;
use serde::{Deserialize, Serialize};
use wire_frame::Framing;

use super::meter::{SizeUsage, UsageEntry};
use tilo_server_protocol::hosted_vm::VmSize;

pub const FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024).reject_empty();

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmRequest {
    pub workspace: String,
    pub environment: String,
    pub account: String,
    pub size: VmSize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShareRequest {
    pub path: PathBuf,
    pub read_only: bool,
}

/// A workspace's VM to wake for its Browser, its daemon started with `material` when it does not
/// run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenBrowser {
    pub workspace: String,
    /// The account the VM belongs to, as the server knows it; opening Browser needs no card, since
    /// its page actions are counted instead.
    pub account: String,
    pub size: VmSize,
    /// Where the session keeps its profile between runs: a folder of the workspace's files, at the
    /// same path in the VM, which it restores from and writes back.
    pub store: Option<PathBuf>,
    /// The host's placement (`tilo_server_protocol::hosted::Material`), in JSON: its ticket is
    /// what the daemon logs in with.
    pub material: String,
}

/// A workspace's VM to wake for its Linux apps, its daemon started with `material` when it does
/// not run; it needs the account's plan and, on a host that sells its VMs, a card.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenApp {
    pub workspace: String,
    pub account: String,
    pub size: VmSize,
    /// The host's placement, as [`OpenBrowser::material`].
    pub material: String,
}

/// New variants go last: the encoding names a variant by its place.
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
    OpenBrowser {
        browser: OpenBrowser,
    },
    StopBrowser {
        workspace: String,
    },
    OpenApp {
        app: OpenApp,
    },
    StopApps {
        workspace: String,
    },
    /// Make the VM `vm`, which must not exist, from the disk of workspace `from`'s VM in the same
    /// environment, as it is now: its tools, its tenant's home. Only its disk: it boots cold, and
    /// keeps nothing of `from`'s sessions. Answered [`Reply::Done`] once its disk is there.
    Branch {
        vm: VmRequest,
        from: String,
    },
    /// Stop the VM and remove it, its disk and its snapshot: it no longer counts toward the
    /// account's VMs. Deleting one there is not is [`Reply::Done`] too.
    Delete {
        workspace: String,
        environment: String,
    },
    /// Move the workspace's VM to the machine listening at `to`: it is suspended, its disk and
    /// snapshot streamed there, and this machine forgets it once that one has all of it.
    MoveVm {
        workspace: String,
        environment: String,
        to: PathBuf,
    },
    /// Copy the workspace's VM to the replica machine listening at `to`, incrementally: the blocks
    /// that changed since its last copy.
    ReplicateVm {
        workspace: String,
        environment: String,
        to: PathBuf,
    },
    /// Drain this machine for a rolling update: take no more wakes and suspend every awake VM onto
    /// the state directory, as the cloud's eviction notice does.
    Drain,
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
    /// What its process holds of the host's memory, as the kernel measured it; `None` when it is
    /// not running.
    pub resident_mib: Option<u64>,
    /// What its balloon holds back from the guest, which the host has.
    pub balloon_mib: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostLoad {
    /// The most its VMs may take together: the node's memory less what the host keeps for itself;
    /// `None` when only its jail's slots bound them.
    pub budget_mib: Option<u64>,
    /// What its awake VMs and the templates it is making are counted at: what the kernel measured,
    /// or what a VM not yet measured is expected to take.
    pub awake_mib: u64,
    /// What wakes waiting for room on it ask for.
    pub waiting_mib: u64,
}

impl HostLoad {
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
        woke_ms: Option<u64>,
    },
    /// The account's plan does not allow it: why, in words for whoever
    /// ran it, and what would let it through.
    Refused(tilo_actions::remote::server_account::Refusal),
    Done,
    Status {
        vms: Vec<VmStatus>,
        load: HostLoad,
    },
    Usage {
        #[serde(with = "usage_on_the_wire")]
        records: Vec<UsageEntry>,
        by_size: Vec<SizeUsage>,
    },
    Error(String),
    /// The VM waits for room, in words for whoever ran it; ask again.
    Busy(String),
    /// A workspace VM's daemon logged in. `opened_ms`: what starting it took, when this request
    /// started it; `device`: the device it logged in as, its key in hex.
    Opened {
        opened_ms: Option<u64>,
        device: String,
    },
    /// Workspace `workspace` has no VM to branch: nothing was made, and the branch's workspace
    /// starts as any new one does, at its first Linux command.
    NoVm {
        workspace: String,
    },
    /// The VM moved, or was copied to the replica, as `bytes` of its state went.
    Moved {
        vm: String,
        bytes: u64,
    },
}

/// The socket's frames are postcard, which encodes neither the log's
/// flattened identity nor its skipped empty fields: on the socket a record
/// travels field by field, every field always present.
mod usage_on_the_wire {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::super::meter::{RunEnd, UsageEntry, UsageKind, VmIdentity};

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

    pub fn serialize<S: Serializer>(records: &[UsageEntry], to: S) -> Result<S::Ok, S::Error> {
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

    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<UsageEntry>, D::Error> {
        Ok(Vec::<Wire>::deserialize(from)?
            .into_iter()
            .map(|wire| UsageEntry {
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

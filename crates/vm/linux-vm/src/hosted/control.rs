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
    Refused(tilo_action_services::server_account::Refusal),
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
                UsageEntry {
                    kind: UsageKind::Running,
                    identity: identity.clone(),
                    start_ms: 1_000,
                    end_ms: 4_500,
                    millis: 3_500,
                    seconds: 3,
                    ended: Some(RunEnd::Suspended),
                    snapshot_bytes: None,
                },
                UsageEntry {
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

    fn payload(request: &Request) -> Vec<u8> {
        let frame = FRAMING.encode_value(request).unwrap();
        frame.get(wire_frame::PREFIX_BYTES..).unwrap().to_vec()
    }

    fn bytes(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// Each request, and what it is on the wire.
    fn encoded() -> Vec<(Request, Vec<u8>)> {
        let vm = VmRequest {
            workspace: "ws".into(),
            environment: "default".into(),
            account: "a".into(),
            size: VmSize::Small,
        };
        let vm_bytes: &[u8] = b"\x02ws\x07default\x01a\x00";
        let named: &[u8] = b"\x02ws\x07default";
        vec![
            (
                Request::Ensure {
                    vm: vm.clone(),
                    shares: vec![ShareRequest {
                        path: "/p".into(),
                        read_only: true,
                    }],
                },
                bytes(&[b"\x00", vm_bytes, b"\x01\x02/p\x01"]),
            ),
            (
                Request::Suspend {
                    workspace: "ws".into(),
                    environment: "default".into(),
                },
                bytes(&[b"\x01", named]),
            ),
            (
                Request::Stop {
                    workspace: "ws".into(),
                    environment: "default".into(),
                },
                bytes(&[b"\x02", named]),
            ),
            (Request::Status, b"\x03".to_vec()),
            (
                Request::Usage {
                    account: "a".into(),
                    from_ms: 1,
                    to_ms: 300,
                },
                b"\x04\x01a\x01\xac\x02".to_vec(),
            ),
            (
                Request::OpenBrowser {
                    browser: OpenBrowser {
                        workspace: "ws".into(),
                        account: "a".into(),
                        size: VmSize::Small,
                        store: Some("/s".into()),
                        material: "m".into(),
                    },
                },
                b"\x05\x02ws\x01a\x00\x01\x02/s\x01m".to_vec(),
            ),
            (
                Request::StopBrowser {
                    workspace: "ws".into(),
                },
                b"\x06\x02ws".to_vec(),
            ),
            (
                Request::OpenApp {
                    app: OpenApp {
                        workspace: "ws".into(),
                        account: "a".into(),
                        size: VmSize::Small,
                        material: "m".into(),
                    },
                },
                b"\x07\x02ws\x01a\x00\x01m".to_vec(),
            ),
            (
                Request::StopApps {
                    workspace: "ws".into(),
                },
                b"\x08\x02ws".to_vec(),
            ),
            (
                Request::Branch {
                    vm,
                    from: "src".into(),
                },
                bytes(&[b"\x09", vm_bytes, b"\x03src"]),
            ),
            (
                Request::Delete {
                    workspace: "ws".into(),
                    environment: "default".into(),
                },
                bytes(&[b"\x0a", named]),
            ),
        ]
    }

    /// Moving a VM away, and draining a machine for a rolling update: the requests a lease or an
    /// operator sends.
    fn moving(named: &[u8]) -> Vec<(Request, Vec<u8>)> {
        vec![
            (
                Request::MoveVm {
                    workspace: "ws".into(),
                    environment: "default".into(),
                    to: "/m".into(),
                },
                bytes(&[b"\x0b", named, b"\x02/m"]),
            ),
            (
                Request::ReplicateVm {
                    workspace: "ws".into(),
                    environment: "default".into(),
                    to: "/m".into(),
                },
                bytes(&[b"\x0c", named, b"\x02/m"]),
            ),
            (Request::Drain, b"\x0d".to_vec()),
        ]
    }

    /// Every request is pinned byte for byte: a change to one is a change to the protocol the
    /// host and its clients (ush, the member, the CLI), all shipped in one image, speak.
    #[test]
    fn every_request_keeps_its_encoding() {
        let named: &[u8] = b"\x02ws\x07default";
        let all = encoded().into_iter().chain(moving(named));
        for (request, expected) in all {
            assert_eq!(payload(&request), expected, "{request:?}");
            let frame = FRAMING.encode_value(&request).unwrap();
            let read: Request = FRAMING.read_value(&mut frame.as_slice()).unwrap().unwrap();
            assert_eq!(read, request);
        }
    }
}

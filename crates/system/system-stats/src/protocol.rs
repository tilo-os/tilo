//! The wire contract between the session's service and an app.
//!
//! The machine's storage (its block devices, their partitions, filesystems
//! and encryption, and each drive's health) is not a reading at an interval
//! but an answer: the app asks with [`Request::Storage`] when it opens and
//! when the person rescans, and is sent one [`Storage`].

use serde::{Deserialize, Serialize};

/// The service's name: its socket is `<services root>/system-stats.sock`
/// and the app finds its descriptor in `TILO_SVC_SYSTEM-STATS_FD`.
pub const SERVICE: &str = "system-stats";

pub const PROTOCOL: u16 = 1;

/// Prefix of the launcher's handoff, before any frame.
pub const HANDOFF_MAGIC: [u8; 4] = *b"QSS1";

/// Frames on the connection: a little-endian `u32` length, then JSON. A
/// snapshot of a few thousand processes stays well inside the limit.
pub const FRAMING: wire_frame::Framing =
    wire_frame::Framing::little_endian(16 << 20).reject_empty();

pub const HANDOFF_FRAMING: wire_frame::Framing =
    wire_frame::Framing::little_endian(4 << 10).reject_empty();

/// The shortest interval a subscription may ask for. Faster than this is
/// not a monitor any more, it is load of its own.
pub const MIN_INTERVAL_MS: u32 = 250;

pub const MAX_INTERVAL_MS: u32 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grant {
    /// Machine-wide readings: processor, memory, pressure, disks, network,
    /// graphics, battery and temperature, and the machine's storage devices
    /// with their health ([`Request::Storage`]). The `system-info` capability.
    SystemInfo,
    /// The running apps and the person's process list (pid, name, app,
    /// user, processor, memory, state), and ending the person's own apps
    /// and processes. The `processes` capability.
    Processes,
    /// A process's command line, beside the list. Asked for separately: a
    /// command line can carry a file name, a host or a token.
    CommandLines,
}

impl Grant {
    /// Every grant, in the order a prompt would ask for them.
    pub const ALL: [Self; 3] = [Self::SystemInfo, Self::Processes, Self::CommandLines];

    /// The device scope a workspace capability names this grant by
    /// (`collab_types::caps::Scope::Device`).
    #[must_use]
    pub const fn device_scope(self) -> &'static str {
        match self {
            Self::SystemInfo => "system-info",
            Self::Processes => "processes",
            Self::CommandLines => "process-commands",
        }
    }

    #[must_use]
    pub fn from_device_scope(scope: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|grant| grant.device_scope() == scope)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub system_info: bool,
    #[serde(default)]
    pub processes: bool,
    #[serde(default)]
    pub command_lines: bool,
}

impl Grants {
    pub const IMAGE_DEFAULT: Self = Self {
        system_info: true,
        processes: true,
        command_lines: false,
    };

    pub const ALL: Self = Self {
        system_info: true,
        processes: true,
        command_lines: true,
    };

    #[must_use]
    pub const fn within(self, ceiling: Self) -> Self {
        Self {
            system_info: self.system_info && ceiling.system_info,
            processes: self.processes && ceiling.processes,
            command_lines: self.command_lines && ceiling.command_lines,
        }
    }

    #[must_use]
    pub const fn holds(self, grant: Grant) -> bool {
        match grant {
            Grant::SystemInfo => self.system_info,
            // A command line is part of a process row: without the list it
            // has nothing to belong to.
            Grant::Processes => self.processes,
            Grant::CommandLines => self.processes && self.command_lines,
        }
    }

    #[must_use]
    pub const fn with(mut self, grant: Grant) -> Self {
        match grant {
            Grant::SystemInfo => self.system_info = true,
            Grant::Processes => self.processes = true,
            Grant::CommandLines => self.command_lines = true,
        }
        self
    }

    #[must_use]
    pub const fn any(self) -> bool {
        self.system_info || self.processes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub protocol: u16,
    pub app_id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// What the launcher granted: the image's default for the apps it ships,
    /// and whatever the room's capabilities name.
    pub grants: Grants,
    /// The most the app may ever be granted on this connection: what its
    /// manifest declares. A prompt never asks past it.
    pub ceiling: Grants,
}

pub fn write_handoff<W: std::io::Write + ?Sized>(
    stream: &mut W,
    handoff: &Handoff,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(handoff).map_err(std::io::Error::other)?;
    stream.write_all(&HANDOFF_MAGIC)?;
    HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

pub fn read_handoff<R: std::io::Read + ?Sized>(stream: &mut R) -> std::io::Result<Handoff> {
    let mut magic = [0_u8; 4];
    stream.read_exact(&mut magic)?;
    if magic != HANDOFF_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a system information handoff",
        ));
    }
    let bytes = HANDOFF_FRAMING
        .read(stream)?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
    let handoff: Handoff = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if handoff.protocol != PROTOCOL {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "system information protocol {} is not served",
                handoff.protocol
            ),
        ));
    }
    Ok(handoff)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Push a snapshot every `interval_ms` under `subscription`, an id the
    /// app picks. `processes` asks for the app list and process list too.
    Subscribe {
        subscription: u64,
        interval_ms: u32,
        #[serde(default)]
        processes: bool,
    },
    Unsubscribe {
        subscription: u64,
    },
    /// Put the permission prompt in front of the person for `grant`. The
    /// answer is [`Reply::Grants`] under `request`.
    AskGrant {
        request: u64,
        grant: Grant,
    },
    EndApp {
        request: u64,
        app: String,
        #[serde(default)]
        force: bool,
    },
    EndProcess {
        request: u64,
        pid: u32,
        started_at: u64,
        #[serde(default)]
        force: bool,
    },
    /// Describe the machine's storage once. The answer is [`Reply::Storage`]
    /// under `request`, or [`Reply::Refused`].
    Storage {
        request: u64,
    },
}

/// What the service answers.
///
/// On the wire an object tagged by `op`, as serde's internally tagged enums
/// are. It is read through a flat struct of every field rather than derived: a derived
/// internally tagged enum buffers the whole reply and reads every field of
/// a snapshot a second time from the buffer, and that second reader, compiled
/// for each type in the snapshot, was a third of Monitor's protocol code.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Reply {
    Snapshot {
        subscription: u64,
        snapshot: Box<Snapshot>,
    },
    /// `subscription` was refused, and nothing will be pushed for it.
    SubscriptionRefused {
        subscription: u64,
        refusal: Refusal,
    },
    Grants {
        request: u64,
        grants: Grants,
    },
    Ended {
        request: u64,
        ended: u32,
    },
    Refused {
        request: u64,
        refusal: Refusal,
    },
    Storage {
        request: u64,
        storage: Box<Storage>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReplyOp {
    Snapshot,
    SubscriptionRefused,
    Grants,
    Ended,
    Refused,
    Storage,
}

/// A [`Reply`] as it arrives: its tag and the fields of every variant, read
/// in one pass straight from the stream.
#[derive(Deserialize)]
struct ReplyWire {
    op: ReplyOp,
    subscription: Option<u64>,
    snapshot: Option<Box<Snapshot>>,
    request: Option<u64>,
    refusal: Option<Refusal>,
    grants: Option<Grants>,
    ended: Option<u32>,
    storage: Option<Box<Storage>>,
}

impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        fn field<T, E: serde::de::Error>(value: Option<T>, name: &'static str) -> Result<T, E> {
            value.ok_or_else(|| E::missing_field(name))
        }
        let wire = ReplyWire::deserialize(deserializer)?;
        Ok(match wire.op {
            ReplyOp::Snapshot => Self::Snapshot {
                subscription: field(wire.subscription, "subscription")?,
                snapshot: field(wire.snapshot, "snapshot")?,
            },
            ReplyOp::SubscriptionRefused => Self::SubscriptionRefused {
                subscription: field(wire.subscription, "subscription")?,
                refusal: field(wire.refusal, "refusal")?,
            },
            ReplyOp::Grants => Self::Grants {
                request: field(wire.request, "request")?,
                grants: field(wire.grants, "grants")?,
            },
            ReplyOp::Ended => Self::Ended {
                request: field(wire.request, "request")?,
                ended: field(wire.ended, "ended")?,
            },
            ReplyOp::Refused => Self::Refused {
                request: field(wire.request, "request")?,
                refusal: field(wire.refusal, "refusal")?,
            },
            ReplyOp::Storage => Self::Storage {
                request: field(wire.request, "request")?,
                storage: field(wire.storage, "storage")?,
            },
        })
    }
}

/// Why something was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Refusal {
    NotGranted {
        grant: Grant,
    },
    NotYours,
    NotAnApp,
    Gone,
    Declined,
    Invalid {
        detail: String,
    },
    /// The operating system refused.
    Failed {
        detail: String,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGranted { grant } => write!(f, "not allowed to read {}", grant.device_scope()),
            Self::NotYours => f.write_str("it belongs to another user"),
            Self::NotAnApp => f.write_str("it is part of the system, not an app"),
            Self::Gone => f.write_str("it is no longer running"),
            Self::Declined => f.write_str("permission was not given"),
            Self::Invalid { detail } | Self::Failed { detail } => f.write_str(detail),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When it was taken, in milliseconds since the Unix epoch.
    pub taken_ms: u64,
    /// The window its rates were measured over, in milliseconds; zero for
    /// the first reading, whose rates are not known yet.
    pub window_ms: u64,
    #[serde(default)]
    pub system: Option<SystemStats>,
    /// Running apps, busiest first; empty without [`Grant::Processes`] or
    /// when the subscription did not ask for processes.
    #[serde(default)]
    pub apps: Vec<AppUsage>,
    #[serde(default)]
    pub processes: Vec<ProcessStats>,
    /// Whether the process list was part of this reading (it may be
    /// genuinely empty when it was).
    #[serde(default)]
    pub processes_included: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemStats {
    pub host: HostInfo,
    #[serde(default)]
    pub cpu: Option<CpuStats>,
    /// The load averages over 1, 5 and 15 minutes.
    pub load_average: [f32; 3],
    pub memory: MemoryStats,
    #[serde(default)]
    pub pressure: Option<Pressure>,
    pub disks: Vec<DiskStats>,
    pub network: NetworkStats,
    pub gpus: Vec<GpuStats>,
    pub batteries: Vec<BatteryStats>,
    pub thermal: Vec<ThermalZone>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    pub kernel: String,
    #[serde(default)]
    pub cpu_model: Option<String>,
    pub logical_cpus: u32,
    pub uptime_secs: u64,
}

/// Processor load in percent of each core, and of all of them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuStats {
    pub total: f32,
    pub per_core: Vec<f32>,
}

/// Memory and swap, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl MemoryStats {
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    #[must_use]
    pub const fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

/// Pressure stall information: the share of the last ten seconds in which
/// some (or all) runnable work waited on the resource, in percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pressure {
    pub cpu: PressureLine,
    pub memory: PressureLine,
    pub io: PressureLine,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PressureLine {
    pub some_avg10: f32,
    #[serde(default)]
    pub full_avg10: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskStats {
    pub device: String,
    #[serde(default)]
    pub mount: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub read_bytes_per_sec: f32,
    pub write_bytes_per_sec: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkStats {
    pub rx_bytes_per_sec: f32,
    pub tx_bytes_per_sec: f32,
    pub interfaces: Vec<InterfaceStats>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InterfaceStats {
    pub name: String,
    pub rx_bytes_per_sec: f32,
    pub tx_bytes_per_sec: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuStats {
    pub driver: String,
    #[serde(default)]
    pub busy_percent: Option<f32>,
    #[serde(default)]
    pub memory_used: Option<u64>,
    #[serde(default)]
    pub memory_total: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BatteryStats {
    pub name: String,
    pub percent: Option<f32>,
    /// `Charging`, `Discharging`, `Full`, `Not charging` or `Unknown`, as the
    /// kernel spells it.
    pub status: String,
    #[serde(default)]
    pub power_watts: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThermalZone {
    pub name: String,
    pub celsius: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppUsage {
    pub app: String,
    pub processes: u32,
    /// Processor use in percent of the whole machine.
    pub cpu: f32,
    pub memory_bytes: u64,
    pub endable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessStats {
    pub pid: u32,
    #[serde(default)]
    pub ppid: Option<u32>,
    /// The process's start time in clock ticks since boot: with `pid`, the
    /// identity of this one lifetime, which an end request repeats.
    pub started_at: u64,
    pub name: String,
    #[serde(default)]
    pub app: Option<String>,
    pub uid: u32,
    pub user: String,
    /// Processor use in percent of the whole machine.
    pub cpu: f32,
    pub memory_bytes: u64,
    pub virtual_bytes: u64,
    pub threads: u32,
    pub state: ProcessState,
    /// Seconds since it started.
    pub run_secs: u64,
    pub kernel_thread: bool,
    #[serde(default)]
    pub command: Option<String>,
    pub endable: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessState {
    Running,
    #[default]
    Sleeping,
    Waiting,
    Stopped,
    Zombie,
    Idle,
    Dead,
}

impl ProcessState {
    #[must_use]
    pub const fn from_code(code: char) -> Self {
        match code {
            'R' => Self::Running,
            'D' => Self::Waiting,
            'T' | 't' => Self::Stopped,
            'Z' => Self::Zombie,
            'I' => Self::Idle,
            'X' | 'x' => Self::Dead,
            _ => Self::Sleeping,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Sleeping => "Sleeping",
            Self::Waiting => "Waiting",
            Self::Stopped => "Stopped",
            Self::Zombie => "Zombie",
            Self::Idle => "Idle",
            Self::Dead => "Dead",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Storage {
    pub devices: Vec<StorageDevice>,
}

/// One block device and the devices stacked on it: a disk's partitions, an
/// encrypted partition's unlocked mapping, a volume group's volumes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StorageDevice {
    /// The name `lsblk` lists it by (`nvme0n1p4`; a mapping's own name, such
    /// as `home`, for a device-mapper device).
    pub name: String,
    /// The kernel's name (`dm-1` for the mapping `home`), which is what
    /// `/dev` and `UDisks2` know it by.
    #[serde(default)]
    pub kernel_name: Option<String>,
    pub kind: String,
    /// In bytes.
    pub size: u64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub rotational: bool,
    #[serde(default)]
    pub removable: bool,
    #[serde(default)]
    pub read_only: bool,
    /// What it holds: a filesystem (`ext4`), swap, or a container such as
    /// `crypto_LUKS`.
    #[serde(default)]
    pub fstype: Option<String>,
    #[serde(default)]
    pub fs_version: Option<String>,
    #[serde(default)]
    pub part_type: Option<String>,
    /// What that type is called (`EFI System`, `Microsoft reserved`).
    #[serde(default)]
    pub part_type_name: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub fs_used: Option<u64>,
    #[serde(default)]
    pub mountpoint: Option<String>,
    #[serde(default)]
    pub children: Vec<StorageDevice>,
    /// What the drive reports of its own health; only on a whole drive, and
    /// only where it reports anything.
    #[serde(default)]
    pub health: Option<DriveHealth>,
}

/// A drive's own account of its health (SMART, or the `NVMe` health log),
/// as `UDisks2` reads it. Every reading is optional: a drive reports what it
/// reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveHealth {
    /// Whether the drive reports health at all (always for `NVMe`; ATA drives
    /// say whether they support SMART).
    pub supported: bool,
    /// When the drive's health was last read, in seconds since the Unix epoch;
    /// `None` when it has not been read yet.
    #[serde(default)]
    pub updated: Option<u64>,
    /// The drive's own verdict: it predicts its failure (ATA), or it raises a
    /// critical warning (`NVMe`).
    #[serde(default)]
    pub failing: bool,
    /// The `NVMe` critical warnings raised, as `UDisks2` names them: `spare`,
    /// `temperature`, `degraded`, `readonly`, `volatile_mem`, `pmr_readonly`.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub temperature_c: Option<i32>,
    #[serde(default)]
    pub power_on_hours: Option<u64>,
    #[serde(default)]
    pub power_cycles: Option<u64>,
    /// `NVMe` wear: the share of the drive's rated life used, in percent (it
    /// may pass 100).
    #[serde(default)]
    pub percent_used: Option<u8>,
    /// `NVMe` spare capacity left, in percent, and the level below which the
    /// drive warns.
    #[serde(default)]
    pub available_spare: Option<u8>,
    #[serde(default)]
    pub spare_threshold: Option<u8>,
    #[serde(default)]
    pub bytes_written: Option<u64>,
    #[serde(default)]
    pub bytes_read: Option<u64>,
    #[serde(default)]
    pub media_errors: Option<u64>,
    #[serde(default)]
    pub unsafe_shutdowns: Option<u64>,
    #[serde(default)]
    pub bad_sectors: Option<u64>,
    #[serde(default)]
    pub failing_attributes: Option<u32>,
    /// The last self-test's outcome as `UDisks2` names it (`success`,
    /// `aborted`, `inprogress`, ...).
    #[serde(default)]
    pub self_test: Option<String>,
}

pub fn write_message<W: std::io::Write + ?Sized, T: Serialize>(
    stream: &mut W,
    message: &T,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    FRAMING.write(stream, &bytes)?;
    stream.flush()
}

pub fn read_message<R: std::io::Read + ?Sized, T: serde::de::DeserializeOwned>(
    stream: &mut R,
) -> std::io::Result<Option<T>> {
    let Some(bytes) = FRAMING.read(stream)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_round_trips_and_a_foreign_one_is_refused() {
        let handoff = Handoff {
            protocol: PROTOCOL,
            app_id: "so.tilo.monitor".into(),
            workspace_id: Some("room".into()),
            grants: Grants::IMAGE_DEFAULT,
            ceiling: Grants::ALL,
        };
        let mut wire = Vec::new();
        write_handoff(&mut wire, &handoff).unwrap();
        assert_eq!(read_handoff(&mut wire.as_slice()).unwrap(), handoff);

        let mut other = wire.clone();
        other[0] = b'X';
        assert!(read_handoff(&mut other.as_slice()).is_err());

        let future = Handoff {
            protocol: PROTOCOL + 1,
            ..handoff
        };
        let mut wire = Vec::new();
        write_handoff(&mut wire, &future).unwrap();
        assert!(read_handoff(&mut wire.as_slice()).is_err());
    }

    /// Every reply reads back as it was written, in the internally tagged
    /// form it always had on the wire, and a reply without a field its `op`
    /// needs is refused.
    #[test]
    fn every_reply_reads_back_from_its_tagged_wire_form() {
        let replies = [
            Reply::Snapshot {
                subscription: 7,
                snapshot: Box::default(),
            },
            Reply::SubscriptionRefused {
                subscription: 7,
                refusal: Refusal::NotGranted {
                    grant: Grant::Processes,
                },
            },
            Reply::Grants {
                request: 3,
                grants: Grants::IMAGE_DEFAULT,
            },
            Reply::Ended {
                request: 4,
                ended: 2,
            },
            Reply::Refused {
                request: 5,
                refusal: Refusal::Invalid {
                    detail: "no such process".into(),
                },
            },
            Reply::Storage {
                request: 6,
                storage: Box::new(Storage {
                    devices: vec![StorageDevice {
                        name: "nvme0n1".into(),
                        kind: "disk".into(),
                        size: 1_024_209_543_168,
                        children: vec![StorageDevice {
                            name: "nvme0n1p4".into(),
                            kind: "part".into(),
                            fstype: Some("crypto_LUKS".into()),
                            fs_version: Some("2".into()),
                            ..StorageDevice::default()
                        }],
                        health: Some(DriveHealth {
                            supported: true,
                            power_on_hours: Some(2294),
                            ..DriveHealth::default()
                        }),
                        ..StorageDevice::default()
                    }],
                }),
            },
        ];
        for reply in replies {
            let wire = serde_json::to_value(&reply).unwrap();
            assert!(wire.get("op").is_some_and(serde_json::Value::is_string));
            let text = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<Reply>(&text).unwrap(), reply);
            assert_eq!(
                serde_json::from_slice::<Reply>(text.as_bytes()).unwrap(),
                reply
            );
        }
        let tag_last = r#"{"request":4,"ended":2,"op":"ended"}"#;
        assert_eq!(
            serde_json::from_str::<Reply>(tag_last).unwrap(),
            Reply::Ended {
                request: 4,
                ended: 2
            }
        );
        assert!(serde_json::from_str::<Reply>(r#"{"op":"ended","request":4}"#).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"op":"restarted","request":4}"#).is_err());
    }

    #[test]
    fn a_command_line_needs_the_process_list_too() {
        let only_commands = Grants {
            command_lines: true,
            ..Grants::default()
        };
        assert!(!only_commands.holds(Grant::CommandLines));
        assert!(!only_commands.any());
        assert!(
            Grants::IMAGE_DEFAULT
                .with(Grant::CommandLines)
                .holds(Grant::CommandLines)
        );
        assert!(!Grants::IMAGE_DEFAULT.holds(Grant::CommandLines));
        for grant in Grant::ALL {
            assert_eq!(Grant::from_device_scope(grant.device_scope()), Some(grant));
        }
    }
}

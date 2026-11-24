//! The `disks` resource: the drives and partitions this computer sees, what each holds and how
//! healthy it is, with the actions that open, close, erase, test and eject them.
//!
//! What reads the block devices and asks UDisks2 is the session's system service. The names a
//! person gives a drive are the workspace's, kept by the Disks app itself.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const DISKS: &str = "disks";

/// What a drive reports of its own health: SMART, or the `NVMe` health log.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DiskHealth {
    /// Whether the drive reports health at all.
    #[serde(default)]
    pub supported: bool,
    /// The drive's own verdict: it predicts its failure, or raises a critical warning.
    #[serde(default)]
    pub failing: bool,
    /// The critical warnings raised: `spare`, `temperature`, `degraded`, `readonly`,
    /// `volatile_mem`, `pmr_readonly`.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub temperature_c: Option<i32>,
    #[serde(default)]
    pub power_on_hours: Option<u64>,
    #[serde(default)]
    pub power_cycles: Option<u64>,
    /// The share of the drive's rated life used, in percent (it may pass 100).
    #[serde(default)]
    pub percent_used: Option<u8>,
    #[serde(default)]
    pub available_spare: Option<u8>,
    #[serde(default)]
    pub bytes_written: Option<u64>,
    #[serde(default)]
    pub bytes_read: Option<u64>,
    #[serde(default)]
    pub media_errors: Option<u64>,
    #[serde(default)]
    pub bad_sectors: Option<u64>,
    /// The last self-test's outcome as UDisks2 names it (`success`, `aborted`, `inprogress`).
    #[serde(default)]
    pub self_test: Option<String>,
}

/// One partition of a drive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Partition {
    /// Its name in `/dev` (`sda1`, `nvme0n1p2`).
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub fstype: Option<String>,
    /// What its partition table calls it (`EFI System`, `Microsoft basic data`).
    #[serde(default)]
    pub part_type_name: Option<String>,
    pub size_bytes: u64,
    #[serde(default)]
    pub used_bytes: Option<u64>,
    #[serde(default)]
    pub mountpoint: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
}

/// One drive and its partitions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Drive {
    /// Its name in `/dev` (`sda`, `nvme0n1`).
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    /// What it is: `disk`, `loop`, `rom` and so on, as the kernel names it.
    pub kind: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub rotational: bool,
    #[serde(default)]
    pub removable: bool,
    #[serde(default)]
    pub read_only: bool,
    /// A filesystem written on the whole drive, when it has no partition table.
    #[serde(default)]
    pub fstype: Option<String>,
    #[serde(default)]
    pub mountpoint: Option<String>,
    pub partitions: Vec<Partition>,
    #[serde(default)]
    pub health: Option<DiskHealth>,
}

/// A drive's or partition's read and seek rates, from the last benchmark.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Benchmark {
    pub device: String,
    /// Sequential reads, in bytes a second.
    pub read_per_sec: f64,
    /// Random 4 KiB reads, in reads a second.
    pub reads_per_sec: f64,
}

/// What this computer's drives read as now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Disks {
    /// Whether the machine's block devices could be read.
    #[serde(default)]
    pub serving: bool,
    /// Why they could not be read, until they can.
    #[serde(default)]
    pub read_error: Option<String>,
    pub drives: Vec<Drive>,
    /// The last benchmark, kept until the next one.
    #[serde(default)]
    pub benchmark: Option<Benchmark>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `disks` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.disks")]
pub enum DiskAction {
    /// Read the drives and partitions again.
    #[effect(kind = "read")]
    #[effect(own_device, icon = "rotate-cw", keywords = "refresh disks, laufwerke neu lesen")]
    Refresh,
    /// Open a partition, or a whole drive, at its filesystem.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "folder-open", keywords = "mount drive, laufwerk einhängen")]
    Mount {
        #[param("The drive's or partition's name in /dev, e.g. sda1")] device: String,
    },
    /// Close a mounted partition.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "eject", keywords = "unmount drive, laufwerk aushängen")]
    Unmount {
        #[param("The partition's name in /dev")] device: String,
    },
    /// Erase a partition, or a whole drive, and write a new filesystem on it.
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, icon = "trash", keywords = "format drive, erase, laufwerk formatieren")]
    Format {
        #[param("The drive's or partition's name in /dev")] device: String,
        #[param("The filesystem to write, e.g. ext4, exfat, vfat")] filesystem: String,
    },
    /// Measure the drive's read speed.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "gauge", keywords = "benchmark drive, laufwerk messen")]
    Benchmark {
        #[param("The drive's or partition's name in /dev")] device: String,
    },
    /// Power a removable drive off so it can be pulled out.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "eject", keywords = "eject drive, laufwerk auswerfen")]
    Eject {
        #[param("The removable drive's name in /dev")] device: String,
    },
}

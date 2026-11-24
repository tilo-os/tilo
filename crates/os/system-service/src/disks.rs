//! The `disks` resource: what [`tilo_system_resources::disks`] says it is, read from the session's
//! system information service and changed through `UDisks2`.
//!
//! Mounting, erasing, ejecting and benchmarking change this machine and nothing else; each lands
//! its answer and a fresh reading.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use tilo_actions::ResourceSpec;
use tilo_service::{offer, unavailable};
use tilo_system_resources::disks::{
    Benchmark, DiskAction, DiskHealth, Disks, Drive, Encryption, Partition,
};
use tilo_system_stats::client::Client;
use tilo_system_stats::protocol::{DriveHealth, StorageDevice};

use crate::device::DiskChange;
use crate::i18n::name;

/// The resource's id.
pub use tilo_system_resources::disks::DISKS;

/// The filesystems a person may write from here.
const FILESYSTEMS: &[&str] = &["ext4", "xfs", "btrfs", "exfat", "vfat", "ntfs"];

/// The `disks` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.disks", label = "Drives")]
pub struct DisksReading;

/// The `disks` resource reading `disks`.
#[must_use]
pub fn resource(disks: &Disks) -> ResourceSpec {
    let mut resource = offer::<DiskAction>(tilo_resource::typed_resource::<DisksReading>(
        DISKS,
        name("so.tilo.system.disks", "Drives"),
    ));
    if !disks.serving {
        for action in ["mount", "unmount", "format", "benchmark", "eject"] {
            unavailable(
                &mut resource,
                action,
                "no_disks",
                "This computer's drives cannot be read",
            );
        }
    }
    resource
}

fn held<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn client() -> &'static Mutex<Option<Client>> {
    static CLIENT: OnceLock<Mutex<Option<Client>>> = OnceLock::new();
    CLIENT.get_or_init(|| Mutex::new(None))
}

fn connect() -> Option<Client> {
    let mut slot = held(client());
    if let Some(client) = slot.as_ref()
        && !client.is_closed()
    {
        return Some(client.clone());
    }
    let socket = tilo_paths::runtime_dir()?
        .join("tilo/services")
        .join(format!("{}.sock", tilo_system_stats::SERVICE));
    let connected = Client::connect_unconfined(&socket, "so.tilo.system-service").ok()?;
    *slot = Some(connected.clone());
    drop(slot);
    Some(connected)
}

/// What this computer's drives read as now.
#[must_use]
pub fn reading() -> Disks {
    let Some(client) = connect() else {
        return Disks {
            read_error: Some("the system information service is not answering".to_owned()),
            ..Disks::default()
        };
    };
    match client.storage() {
        Ok(storage) => Disks {
            serving: true,
            drives: storage.devices.iter().map(drive_of).collect(),
            ..Disks::default()
        },
        Err(refusal) => Disks {
            read_error: Some(refusal.to_string()),
            ..Disks::default()
        },
    }
}

fn drive_of(device: &StorageDevice) -> Drive {
    let partitions = device
        .children
        .iter()
        .map(partition_of)
        .collect();
    Drive {
        name: device.name.clone(),
        model: device.model.clone(),
        serial: device.serial.clone(),
        transport: device.transport.clone(),
        kind: device.kind.clone(),
        size_bytes: device.size,
        rotational: device.rotational,
        removable: device.removable,
        read_only: device.read_only,
        fstype: device.fstype.clone(),
        mountpoint: device.mountpoint.clone(),
        partitions,
        health: device.health.as_ref().map(health_of),
    }
}

fn encryption_of(device: &StorageDevice) -> Option<Encryption> {
    let scheme = match device.fstype.as_deref()? {
        "crypto_LUKS" => match device.fs_version.as_deref() {
            Some(version) if !version.is_empty() => format!("LUKS{version}"),
            _ => "LUKS".to_owned(),
        },
        "BitLocker" => "BitLocker".to_owned(),
        _ => return None,
    };
    Some(Encryption {
        scheme,
        unlocked_as: unlocked_mapping(device).map(|mapping| {
            mapping
                .kernel_name
                .clone()
                .unwrap_or_else(|| mapping.name.clone())
        }),
    })
}

/// The device's unlocked mapping, where it is an encryption that is open.
fn unlocked_mapping(device: &StorageDevice) -> Option<&StorageDevice> {
    device.children.iter().find(|child| child.kind == "crypt")
}

/// One partition. An encrypted one takes its filesystem, label and mount point from the mapping
/// it is unlocked as, and has none while it is locked.
fn partition_of(device: &StorageDevice) -> Partition {
    let encryption = encryption_of(device);
    let holds = match &encryption {
        Some(Encryption {
            unlocked_as: Some(_),
            ..
        }) => unlocked_mapping(device),
        Some(_) => None,
        None => Some(device),
    };
    Partition {
        name: device.name.clone(),
        label: holds
            .and_then(|filesystem| filesystem.label.clone())
            .or_else(|| device.label.clone()),
        fstype: holds.and_then(|filesystem| filesystem.fstype.clone()),
        part_type_name: device.part_type_name.clone(),
        size_bytes: device.size,
        used_bytes: holds
            .and_then(|filesystem| filesystem.fs_used)
            .filter(|used| device.size > 0 && *used <= device.size),
        mountpoint: holds.and_then(|filesystem| filesystem.mountpoint.clone()),
        uuid: device.uuid.clone(),
        encryption,
    }
}

fn health_of(health: &DriveHealth) -> DiskHealth {
    DiskHealth {
        supported: health.supported,
        failing: health.failing,
        warnings: health.warnings.clone(),
        temperature_c: health.temperature_c,
        power_on_hours: health.power_on_hours,
        power_cycles: health.power_cycles,
        percent_used: health.percent_used,
        available_spare: health.available_spare,
        bytes_written: health.bytes_written,
        bytes_read: health.bytes_read,
        media_errors: health.media_errors,
        bad_sectors: health.bad_sectors,
        self_test: health.self_test.clone(),
    }
}

/// A device as `UDisks2` and `/dev` name it: one path segment, never a flag.
fn device_name(device: &str) -> Result<&str, String> {
    let device = device.trim();
    if device.is_empty()
        || device.starts_with('-')
        || device.contains('/')
        || !device
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err(format!("`{device}` is not a device"));
    }
    Ok(device)
}

fn udisks() -> Result<block_devices::udisks::Udisks, String> {
    block_devices::udisks::Udisks::system().map_err(|error| format!("system bus: {error}"))
}

/// Make `change` on the drives.
///
/// # Errors
/// Why it did not happen.
pub fn change(change: &DiskChange) -> Result<(), String> {
    match change {
        DiskChange::Mount(device) => udisks()?
            .mount(device_name(device)?)
            .map(drop)
            .map_err(|error| format!("udisks2 Mount: {error}")),
        DiskChange::Unmount(device) => udisks()?
            .unmount(device_name(device)?)
            .map_err(|error| format!("udisks2 Unmount: {error}")),
        DiskChange::Format { device, filesystem } => {
            let filesystem = filesystem.trim();
            if !FILESYSTEMS.contains(&filesystem) {
                return Err(format!(
                    "`{filesystem}` is not a filesystem written here; use {}",
                    FILESYSTEMS.join(", ")
                ));
            }
            udisks()?
                .format(device_name(device)?, filesystem)
                .map_err(|error| format!("udisks2 Format: {error}"))
        }
        DiskChange::Eject(device) => udisks()?
            .power_off(device_name(device)?)
            .map_err(|error| format!("udisks2 PowerOff: {error}")),
    }
}

/// Measure the device's sequential and random reads; the answer is kept as the reading's
/// benchmark.
///
/// # Errors
/// Why it could not be measured.
pub fn benchmark(device: &str) -> Result<Benchmark, String> {
    use std::io::{Read, Seek, SeekFrom};

    let device = device_name(device)?;
    let path = format!("/dev/{device}");
    let mut file = std::fs::File::open(&path).map_err(|error| format!("open {path}: {error}"))?;
    let mut buffer = vec![0u8; 1 << 20];
    let start = std::time::Instant::now();
    let mut total = 0usize;
    while total < (64 << 20) {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => total = total.saturating_add(read),
            Err(error) => return Err(format!("read {path}: {error}")),
        }
    }
    if total == 0 {
        return Err(format!("read {path}: the device returned no data"));
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "a throughput read to show a person, where a byte count past 2^53 is irrelevant"
    )]
    let read_per_sec = (total as f64 / start.elapsed().as_secs_f64().max(1e-9)).max(0.0);
    let size = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("seek {path}: {error}"))?;
    let mut random = Vec::new();
    if size >= 4096 {
        let start = std::time::Instant::now();
        let mut sample = [0u8; 4096];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..100 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let max_block = size.saturating_sub(4096) / 4096;
            let block = seed.checked_rem(max_block).unwrap_or(0);
            let at = block.saturating_mul(4096);
            if file.seek(SeekFrom::Start(at)).is_ok() && file.read_exact(&mut sample).is_ok() {
                random.push(());
            }
        }
        let elapsed = start.elapsed().as_secs_f64().max(1e-9);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a count of reads read to show a person"
        )]
        let random_reads = (random.len() as f64 / elapsed).max(0.0);
        return Ok(Benchmark {
            device: device.to_owned(),
            read_per_sec,
            reads_per_sec: random_reads,
        });
    }
    Ok(Benchmark {
        device: device.to_owned(),
        read_per_sec,
        reads_per_sec: 0.0,
    })
}

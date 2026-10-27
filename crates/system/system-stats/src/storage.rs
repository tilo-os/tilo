//! The machine's storage, as [`Request::Storage`](crate::protocol::Request::Storage)
//! answers it.
//!
//! An app's sandbox has a near-empty `/dev`, no `/run/udev`, none of the
//! host's programs and no system bus, so it cannot list the machine's disks
//! itself, and widening it would hand every app the machine. The session lists
//! them instead: `lsblk` reads sysfs and the udev database, which the person
//! may read without privilege, and `UDisks2` reports each drive's health (the
//! properties of `org.freedesktop.UDisks2.NVMe.Controller` and
//! `org.freedesktop.UDisks2.Drive.Ata`, and the `NVMe` health log through
//! `SmartGetAttributes`), which polkit lets any local user read.
//!
//! Nothing here changes a device. Mounting, formatting and powering off stay
//! `UDisks2` calls behind polkit.

use crate::protocol::{DriveHealth, Refusal, Storage, StorageDevice};

/// Where the session's storage answers come from.
pub trait StorageSource: Send + Sync {
    /// Describe the machine's storage now.
    ///
    /// # Errors
    /// Why it could not be described.
    fn read(&self) -> Result<Storage, Refusal>;
}

/// The columns [`from_block_devices`] reads, for `lsblk --output`.
pub const LSBLK_COLUMNS: &str = concat!(
    "NAME,KNAME,MODEL,SERIAL,SIZE,TYPE,ROTA,TRAN,RM,RO,FSTYPE,FSVER,",
    "LABEL,MOUNTPOINT,FSUSED,UUID,PARTTYPE,PARTTYPENAME"
);

/// Storage as the session serves it from `lsblk --json --bytes --output
/// LSBLK_COLUMNS` output, with `health` asked of every whole disk: what a
/// capture replays from a real machine's text. `None` when the text is not
/// `lsblk`'s, and where the session reads no `lsblk` (off Linux).
pub fn from_lsblk(
    lsblk: &[u8],
    health: &mut dyn FnMut(&str) -> Option<DriveHealth>,
) -> Option<Storage> {
    #[cfg(target_os = "linux")]
    {
        let devices = block_devices::lsblk::parse_slice(lsblk)?;
        Some(from_block_devices(&devices, health))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (lsblk, health);
        None
    }
}

/// `lsblk`'s device tree as the protocol's, with `health` asked of every
/// whole disk (by its kernel name).
#[cfg(target_os = "linux")]
pub fn from_block_devices(
    devices: &[block_devices::BlockDevice],
    health: &mut dyn FnMut(&str) -> Option<DriveHealth>,
) -> Storage {
    Storage {
        devices: devices
            .iter()
            .map(|device| device_from(device, health))
            .collect(),
    }
}

#[cfg(target_os = "linux")]
fn device_from(
    device: &block_devices::BlockDevice,
    health: &mut dyn FnMut(&str) -> Option<DriveHealth>,
) -> StorageDevice {
    let kernel_name = device.kname.clone();
    let drive_health = if device.kind == "disk" {
        health(kernel_name.as_deref().unwrap_or(&device.name))
    } else {
        None
    };
    StorageDevice {
        name: device.name.clone(),
        kernel_name,
        kind: device.kind.clone(),
        size: device.size,
        model: device.model.clone(),
        serial: device.serial.clone(),
        transport: device.tran.clone(),
        rotational: device.rotational,
        removable: device.removable,
        read_only: device.read_only,
        fstype: device.fstype.clone(),
        fs_version: device.fsver.clone(),
        part_type: device.parttype.clone(),
        part_type_name: device.parttypename.clone(),
        label: device.label.clone(),
        uuid: device.uuid.clone(),
        fs_used: device.fsused,
        mountpoint: device.mountpoint.clone(),
        children: device
            .children
            .iter()
            .map(|child| device_from(child, health))
            .collect(),
        health: drive_health,
    }
}

/// This machine: `lsblk`, and `UDisks2` for health where the session was
/// built with it (feature `udisks`).
pub struct MachineStorage;

#[cfg(target_os = "linux")]
impl StorageSource for MachineStorage {
    fn read(&self) -> Result<Storage, Refusal> {
        let devices = block_devices::Lsblk::new(LSBLK_COLUMNS)
            .list()
            .ok_or_else(|| Refusal::Failed {
                detail: "the storage devices could not be listed".into(),
            })?;
        #[cfg(feature = "udisks")]
        {
            let health = udisks::Health::system();
            Ok(from_block_devices(&devices, &mut |device| {
                health.as_ref()?.of(device)
            }))
        }
        #[cfg(not(feature = "udisks"))]
        {
            Ok(from_block_devices(&devices, &mut |_| None))
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl StorageSource for MachineStorage {
    fn read(&self) -> Result<Storage, Refusal> {
        Err(Refusal::Failed {
            detail: "storage devices are not described on this system".into(),
        })
    }
}

/// Drive health from `UDisks2`, read-only.
#[cfg(all(target_os = "linux", feature = "udisks"))]
pub mod udisks {
    use std::collections::HashMap;

    use block_devices::udisks::{IFACE_BLOCK, SERVICE, object_path};
    use zbus::blocking::Connection;
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    use crate::protocol::DriveHealth;

    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const NVME: &str = "org.freedesktop.UDisks2.NVMe.Controller";
    const ATA: &str = "org.freedesktop.UDisks2.Drive.Ata";

    /// One interface's properties, or a method's `a{sv}` answer.
    pub type Props = HashMap<String, OwnedValue>;

    /// A system-bus connection that reads drive health.
    pub struct Health {
        connection: Connection,
    }

    impl Health {
        /// Connect to the system bus; `None` when there is none.
        #[must_use]
        pub fn system() -> Option<Self> {
            match Connection::system() {
                Ok(connection) => Some(Self { connection }),
                Err(error) => {
                    tracing::debug!(%error, "drive health unavailable: no system bus");
                    None
                }
            }
        }

        /// The health of the drive behind the block device `device` (a
        /// kernel name); `None` when it is no drive (zram, loop) or `UDisks2`
        /// knows no health for it.
        #[must_use]
        pub fn of(&self, device: &str) -> Option<DriveHealth> {
            let block = self.all(&object_path(device), IFACE_BLOCK)?;
            let drive = get::<ObjectPath<'_>>(&block, "Drive")?.to_string();
            if drive == "/" {
                return None;
            }
            if let Some(controller) = self.all(&drive, NVME) {
                return Some(nvme(&controller, self.nvme_log(&drive).as_ref()));
            }
            self.all(&drive, ATA).map(|ata_props| ata(&ata_props))
        }

        fn all(&self, path: &str, interface: &str) -> Option<Props> {
            let reply = self
                .connection
                .call_method(
                    Some(SERVICE),
                    path,
                    Some(PROPERTIES),
                    "GetAll",
                    &(interface,),
                )
                .ok()?;
            let props: Props = reply.body().deserialize().ok()?;
            (!props.is_empty()).then_some(props)
        }

        /// The `NVMe` health log, as `UDisks2` last read it from the drive.
        fn nvme_log(&self, drive: &str) -> Option<Props> {
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            let reply = self
                .connection
                .call_method(
                    Some(SERVICE),
                    drive,
                    Some(NVME),
                    "SmartGetAttributes",
                    &(options,),
                )
                .ok()?;
            reply.body().deserialize().ok()
        }
    }

    /// A value with any variant wrapping taken off.
    fn plain<'v>(value: &'v Value<'v>) -> &'v Value<'v> {
        match value {
            Value::Value(inner) => plain(inner),
            other => other,
        }
    }

    fn get<'p, T>(props: &'p Props, key: &str) -> Option<T>
    where
        T: TryFrom<&'p Value<'p>>,
    {
        let value: &'p Value<'p> = props.get(key)?;
        T::try_from(plain(value)).ok()
    }

    /// A temperature in kelvin as whole degrees Celsius; zero means unknown.
    fn celsius(kelvin: f64) -> Option<i32> {
        (kelvin > 0.0).then(|| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a drive temperature in degrees fits an i32"
            )]
            let degrees = (kelvin - 273.15).round() as i32;
            degrees
        })
    }

    /// Seconds since the epoch, where zero means never.
    fn when(seconds: Option<u64>) -> Option<u64> {
        seconds.filter(|&s| s > 0)
    }

    /// `org.freedesktop.UDisks2.NVMe.Controller`'s properties and, when it
    /// could be read, the health log (`SmartGetAttributes`).
    #[must_use]
    pub fn nvme(controller: &Props, log: Option<&Props>) -> DriveHealth {
        let warnings: Vec<String> = controller
            .get("SmartCriticalWarning")
            .map(|value| match plain(value) {
                Value::Array(items) => items
                    .iter()
                    .filter_map(|item| String::try_from(plain(item)).ok())
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        let log_u64 = |key: &str| log.and_then(|log| get::<u64>(log, key));
        let log_u8 = |key: &str| log.and_then(|log| get::<u8>(log, key));
        let updated = when(get::<u64>(controller, "SmartUpdated"));
        DriveHealth {
            supported: true,
            updated,
            failing: !warnings.is_empty(),
            warnings,
            temperature_c: get::<u16>(controller, "SmartTemperature")
                .and_then(|kelvin| celsius(f64::from(kelvin))),
            power_on_hours: updated.and_then(|_| get::<u64>(controller, "SmartPowerOnHours")),
            power_cycles: log_u64("power_cycles"),
            percent_used: log_u8("percent_used"),
            available_spare: log_u8("avail_spare"),
            spare_threshold: log_u8("spare_thresh"),
            bytes_written: log_u64("total_data_written"),
            bytes_read: log_u64("total_data_read"),
            media_errors: log_u64("media_errors"),
            unsafe_shutdowns: log_u64("unsafe_shutdowns"),
            bad_sectors: None,
            failing_attributes: None,
            self_test: get::<String>(controller, "SmartSelftestStatus")
                .filter(|status| !status.is_empty()),
        }
    }

    /// `org.freedesktop.UDisks2.Drive.Ata`'s properties.
    #[must_use]
    pub fn ata(props: &Props) -> DriveHealth {
        let supported = get::<bool>(props, "SmartSupported").unwrap_or(false);
        let updated = when(get::<u64>(props, "SmartUpdated"));
        // UDisks2 reports -1 for a count it does not know.
        let count = |key: &str| get::<i64>(props, key).and_then(|n| u64::try_from(n).ok());
        DriveHealth {
            supported,
            updated,
            failing: get::<bool>(props, "SmartFailing").unwrap_or(false),
            warnings: Vec::new(),
            temperature_c: get::<f64>(props, "SmartTemperature").and_then(celsius),
            power_on_hours: updated
                .and_then(|_| get::<u64>(props, "SmartPowerOnSeconds"))
                .and_then(|seconds| seconds.checked_div(3600)),
            power_cycles: None,
            percent_used: None,
            available_spare: None,
            spare_threshold: None,
            bytes_written: None,
            bytes_read: None,
            media_errors: None,
            unsafe_shutdowns: None,
            bad_sectors: count("SmartNumBadSectors"),
            failing_attributes: get::<i32>(props, "SmartNumAttributesFailing")
                .and_then(|n| u32::try_from(n).ok()),
            self_test: get::<String>(props, "SmartSelftestStatus")
                .filter(|status| !status.is_empty()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn props(entries: Vec<(&str, Value<'static>)>) -> Props {
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), OwnedValue::try_from(value).unwrap()))
                .collect()
        }

        /// What `UDisks2` reported for the Yoga's Samsung `NVMe` drive
        /// (busctl, 2026-10-22), read into the drive's health.
        #[test]
        fn the_yogas_nvme_health_reads_from_udisks_properties_and_log() {
            let controller = props(vec![
                ("SmartCriticalWarning", Value::from(Vec::<String>::new())),
                ("SmartPowerOnHours", Value::from(2294_u64)),
                ("SmartSelftestStatus", Value::from("success")),
                ("SmartTemperature", Value::from(316_u16)),
                ("SmartUpdated", Value::from(1_792_637_448_u64)),
            ]);
            let log = props(vec![
                ("avail_spare", Value::from(100_u8)),
                ("spare_thresh", Value::from(10_u8)),
                ("percent_used", Value::from(5_u8)),
                ("total_data_read", Value::from(42_203_914_752_000_u64)),
                ("total_data_written", Value::from(44_492_143_104_000_u64)),
                ("power_cycles", Value::from(376_u64)),
                ("unsafe_shutdowns", Value::from(50_u64)),
                ("media_errors", Value::from(0_u64)),
            ]);
            let health = nvme(&controller, Some(&log));
            assert!(health.supported);
            assert!(!health.failing, "no critical warning raised");
            assert_eq!(health.temperature_c, Some(43));
            assert_eq!(health.power_on_hours, Some(2294));
            assert_eq!(health.power_cycles, Some(376));
            assert_eq!(health.percent_used, Some(5));
            assert_eq!(health.available_spare, Some(100));
            assert_eq!(health.bytes_written, Some(44_492_143_104_000));
            assert_eq!(health.media_errors, Some(0));
            assert_eq!(health.self_test.as_deref(), Some("success"));

            let warned = props(vec![
                (
                    "SmartCriticalWarning",
                    Value::from(vec!["spare".to_owned()]),
                ),
                ("SmartUpdated", Value::from(1_u64)),
            ]);
            let health = nvme(&warned, None);
            assert!(health.failing);
            assert_eq!(health.warnings, ["spare"]);
            assert_eq!(health.percent_used, None, "no log, no wear reading");
        }

        /// An ATA drive's verdict, temperature and counts; -1 is unknown.
        #[test]
        fn an_ata_drives_health_reads_from_its_properties() {
            let health = ata(&props(vec![
                ("SmartSupported", Value::from(true)),
                ("SmartUpdated", Value::from(1_792_637_448_u64)),
                ("SmartFailing", Value::from(false)),
                ("SmartTemperature", Value::from(312.15_f64)),
                ("SmartPowerOnSeconds", Value::from(36_000_u64)),
                ("SmartNumBadSectors", Value::from(8_i64)),
                ("SmartNumAttributesFailing", Value::from(-1_i32)),
            ]));
            assert!(health.supported && !health.failing);
            assert_eq!(health.temperature_c, Some(39));
            assert_eq!(health.power_on_hours, Some(10));
            assert_eq!(health.bad_sectors, Some(8));
            assert_eq!(health.failing_attributes, None);
        }
    }
}

/// A fixed answer, for tests and captures.
pub struct FixedStorage(pub Storage);

impl StorageSource for FixedStorage {
    fn read(&self) -> Result<Storage, Refusal> {
        Ok(self.0.clone())
    }
}

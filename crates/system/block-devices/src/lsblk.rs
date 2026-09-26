//! `lsblk -J -b` as a typed device tree.
//!
//! Each tool asks for exactly the columns it reads ([`Lsblk::new`]); a column
//! that was not requested simply parses as absent. Parsing is a pure function
//! over the JSON document ([`parse`]), so callers test their classification
//! against captured output with no real `lsblk`.

use std::time::Duration;

use serde_json::Value;

/// One `lsblk` row and the devices stacked on it (partitions, LUKS mappings,
/// LVM volumes). Absent, `null` and blank columns read as `None`, `0`, `false`
/// or empty; strings are trimmed of lsblk's padding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockDevice {
    /// `NAME`: the kernel name (`sda1`), or the full path (`/dev/sda1`) when
    /// listed with [`Lsblk::full_paths`].
    pub name: String,
    /// `TYPE`: `disk`, `part`, `crypt`, `lvm`, `loop`, `rom`, ...
    pub kind: String,
    /// `SIZE` in bytes.
    pub size: u64,
    /// `START`: offset in 512-byte sectors, whatever the logical sector size.
    pub start: Option<u64>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub wwn: Option<String>,
    /// `TRAN`: `nvme`, `sata`, `usb`, ...
    pub tran: Option<String>,
    /// `ROTA`.
    pub rotational: bool,
    /// `RM`.
    pub removable: bool,
    /// `RO`.
    pub read_only: bool,
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub uuid: Option<String>,
    /// `FSUSED` in bytes.
    pub fsused: Option<u64>,
    /// `MOUNTPOINT`: the one mount lsblk picked when a filesystem has several.
    pub mountpoint: Option<String>,
    /// `MOUNTPOINTS`: every mount of the device.
    pub mountpoints: Vec<String>,
    pub parttype: Option<String>,
    pub partuuid: Option<String>,
    pub pttype: Option<String>,
    pub ptuuid: Option<String>,
    /// `LOG-SEC`: logical sector size in bytes.
    pub log_sec: Option<u64>,
    /// `MAJ:MIN`.
    pub maj_min: Option<String>,
    pub children: Vec<BlockDevice>,
}

impl BlockDevice {
    /// Build a device from one `blockdevices` entry.
    pub fn from_json(value: &Value) -> Self {
        Self {
            name: string(value, "name").unwrap_or_default(),
            kind: string(value, "type").unwrap_or_default(),
            size: number(value, "size").unwrap_or(0),
            start: number(value, "start"),
            model: string(value, "model"),
            serial: string(value, "serial"),
            wwn: string(value, "wwn"),
            tran: string(value, "tran"),
            rotational: flag(value, "rota"),
            removable: flag(value, "rm"),
            read_only: flag(value, "ro"),
            fstype: string(value, "fstype"),
            label: string(value, "label"),
            uuid: string(value, "uuid"),
            fsused: number(value, "fsused"),
            mountpoint: string(value, "mountpoint"),
            mountpoints: value
                .get("mountpoints")
                .and_then(Value::as_array)
                .map(|points| {
                    points
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|point| !point.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            parttype: string(value, "parttype"),
            partuuid: string(value, "partuuid"),
            pttype: string(value, "pttype"),
            ptuuid: string(value, "ptuuid"),
            log_sec: number(value, "log-sec"),
            maj_min: string(value, "maj:min"),
            children: value
                .get("children")
                .and_then(Value::as_array)
                .map(|children| children.iter().map(Self::from_json).collect())
                .unwrap_or_default(),
        }
    }

    /// Every mount of this device: `MOUNTPOINTS` when it was listed, else the
    /// single `MOUNTPOINT`.
    pub fn all_mountpoints(&self) -> Vec<&str> {
        if self.mountpoints.is_empty() {
            self.mountpoint.as_deref().into_iter().collect()
        } else {
            self.mountpoints.iter().map(String::as_str).collect()
        }
    }

    /// This device followed by all of its descendants, depth first.
    pub fn descendants(&self) -> impl Iterator<Item = &BlockDevice> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let device = stack.pop()?;
            stack.extend(device.children.iter().rev());
            Some(device)
        })
    }
}

/// Parse an `lsblk -J` document. `None` when it has no `blockdevices` array
/// (malformed output); `Some(vec![])` when lsblk listed no devices.
pub fn parse(json: &Value) -> Option<Vec<BlockDevice>> {
    let devices = json.get("blockdevices")?.as_array()?;
    Some(devices.iter().map(BlockDevice::from_json).collect())
}

/// [`parse`] over raw `lsblk -J` output.
pub fn parse_slice(output: &[u8]) -> Option<Vec<BlockDevice>> {
    parse(&serde_json::from_slice(output).ok()?)
}

/// One `lsblk -J -b` invocation: the columns a tool reads and the bounds on
/// the child process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lsblk {
    columns: String,
    full_paths: bool,
    output_limit: usize,
    timeout: Duration,
}

impl Lsblk {
    /// List `columns` (lsblk's comma-separated `--output` names) with a 4 MiB
    /// output cap and a 10 second deadline.
    pub fn new(columns: &str) -> Self {
        Self {
            columns: columns.to_owned(),
            full_paths: false,
            output_limit: 4 * 1024 * 1024,
            timeout: Duration::from_secs(10),
        }
    }

    /// `-p`: name devices by full path (`/dev/sda1`).
    #[must_use]
    pub fn full_paths(mut self, on: bool) -> Self {
        self.full_paths = on;
        self
    }

    #[must_use]
    pub fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The argument vector passed to `lsblk`.
    pub fn args(&self) -> Vec<&str> {
        let mut args = vec!["-J", "-b"];
        if self.full_paths {
            args.push("-p");
        }
        args.extend(["--output", self.columns.as_str()]);
        args
    }

    /// Run lsblk and return its stdout, or `None` when it is missing, fails
    /// or exceeds its bounds.
    pub fn output(&self) -> Option<Vec<u8>> {
        let program = crate::system_tool("lsblk");
        let out = bounded_command::run(
            &program,
            &self.args(),
            None,
            self.output_limit,
            self.timeout,
        )
        .ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// Run lsblk and parse its devices; `None` when it could not be run or
    /// its output is malformed.
    pub fn list(&self) -> Option<Vec<BlockDevice>> {
        parse_slice(&self.output()?)
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `-b` sizes are JSON numbers; older lsblk emits them as strings.
fn number(value: &Value, key: &str) -> Option<u64> {
    let value = value.get(key)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// Flags are JSON booleans; older lsblk emits `"0"`/`"1"` strings or numbers.
fn flag(value: &Value, key: &str) -> bool {
    let Some(value) = value.get(key) else {
        return false;
    };
    value
        .as_bool()
        .or_else(|| value.as_u64().map(|n| n != 0))
        .or_else(|| match value.as_str()? {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(json: &str) -> Vec<BlockDevice> {
        parse_slice(json.as_bytes()).expect("fixture is an lsblk document")
    }

    fn find<'a>(devices: &'a [BlockDevice], name: &str) -> &'a BlockDevice {
        devices
            .iter()
            .flat_map(BlockDevice::descendants)
            .find(|device| device.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    #[test]
    fn parses_captured_disks_imager_and_installer_inventories() {
        // Phase `parses_a_captured_disks_inventory`.
        {
            let devices = fixture(include_str!("../tests/fixtures/disks.json"));
            let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, ["zram0", "nvme0n1", "nvme1n1"]);

            let zram = find(&devices, "zram0");
            assert_eq!(zram.kind, "disk");
            assert_eq!(zram.size, 42_949_672_960);
            assert_eq!(zram.model, None, "null model is absent");
            assert_eq!(zram.fstype.as_deref(), Some("swap"));
            assert_eq!(zram.mountpoint.as_deref(), Some("[SWAP]"));
            assert!(zram.children.is_empty());

            let nvme = find(&devices, "nvme0n1");
            assert_eq!(nvme.model.as_deref(), Some("CT2000P310SSD8"));
            assert_eq!(nvme.tran.as_deref(), Some("nvme"));
            assert!(!nvme.rotational && !nvme.removable);
            assert_eq!(nvme.children.len(), 3);

            let root = find(&devices, "nvme0n1p3");
            assert_eq!(root.kind, "part");
            assert_eq!(root.fstype.as_deref(), Some("btrfs"));
            assert_eq!(root.fsused, Some(2_445_203_636_224));
            assert!(
                root.fsused > Some(root.size),
                "pool usage exceeds the member"
            );
            assert_eq!(root.mountpoint.as_deref(), Some("/run/media/system/fedora"));

            let unmounted = find(&devices, "nvme1n1p2");
            assert_eq!(unmounted.mountpoint, None);
            assert_eq!(unmounted.fsused, None);
            assert!(unmounted.all_mountpoints().is_empty());
        }
        // Phase `parses_a_captured_imager_inventory`.
        {
            let devices = fixture(include_str!("../tests/fixtures/imager.json"));
            let nvme = find(&devices, "nvme1n1");
            assert_eq!(nvme.model.as_deref(), Some("WD PC SN560 SDDPNQE-1T00-1002"));
            assert!(nvme.wwn.as_deref().is_some_and(|w| w.starts_with("eui.")));
            assert!(nvme.serial.is_some());
            assert_eq!(
                find(&devices, "nvme0n1p1").mountpoint.as_deref(),
                Some("/boot/efi")
            );
        }
        // Phase `parses_a_captured_installer_inventory_with_every_mount`.
        {
            let devices = fixture(include_str!("../tests/fixtures/installer.json"));
            let disk = find(&devices, "/dev/nvme0n1");
            assert_eq!(disk.pttype.as_deref(), Some("gpt"));
            assert!(disk.ptuuid.is_some());
            assert_eq!(disk.log_sec, Some(512));
            assert_eq!(disk.maj_min.as_deref(), Some("259:0"));
            assert!(!disk.read_only);

            let esp = find(&devices, "/dev/nvme0n1p1");
            assert_eq!(esp.start, Some(2048));
            assert_eq!(
                esp.parttype.as_deref(),
                Some("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
            );

            let root = find(&devices, "/dev/nvme0n1p3");
            let mounts = root.all_mountpoints();
            assert!(mounts.contains(&"/sysroot"), "{mounts:?}");
            assert!(mounts.contains(&"/var/home"), "{mounts:?}");
            assert_eq!(root.mountpoint, None, "MOUNTPOINT was not requested");

            let zram = find(&devices, "/dev/zram0");
            assert_eq!(zram.all_mountpoints(), ["[SWAP]"]);
        }
    }

    #[test]
    fn parser_accepts_older_shapes_walks_holders_and_distinguishes_malformed_documents() {
        // Phase `accepts_the_value_shapes_older_lsblk_emits`.
        {
            let devices = fixture(
                r#"{"blockdevices":[{"name":"sdc","model":"Old Stick  ","size":"8000000000",
                    "type":"disk","rota":"0","rm":"1","ro":1,"fsused":"12",
                    "mountpoint":"  ","mountpoints":[null," /media "]}]}"#,
            );
            let stick = &devices[0];
            assert_eq!(stick.size, 8_000_000_000);
            assert_eq!(stick.model.as_deref(), Some("Old Stick"));
            assert!(!stick.rotational);
            assert!(stick.removable);
            assert!(stick.read_only);
            assert_eq!(stick.fsused, Some(12));
            assert_eq!(stick.mountpoint, None, "blank is absent");
            assert_eq!(stick.mountpoints, ["/media"]);
        }
        // Phase `malformed_and_empty_documents_are_distinguished`.
        {
            assert_eq!(parse_slice(br#"{"blockdevices":[]}"#), Some(Vec::new()));
            assert_eq!(parse_slice(b"{}"), None);
            assert_eq!(parse_slice(b"not json"), None);
        }
        // Phase `descendants_walk_the_holder_tree_depth_first`.
        {
            let devices = fixture(
                r#"{"blockdevices":[{"name":"sda","type":"disk","children":[
                    {"name":"sda1","type":"part","children":[
                        {"name":"luks","type":"crypt","children":[{"name":"root","type":"lvm"}]}]},
                    {"name":"sda2","type":"part"}]}]}"#,
            );
            let order: Vec<_> = devices[0].descendants().map(|d| d.name.as_str()).collect();
            assert_eq!(order, ["sda", "sda1", "luks", "root", "sda2"]);
        }
    }

    #[test]
    fn arguments_use_the_portable_output_form() {
        let query = Lsblk::new("NAME,SIZE").full_paths(true);
        assert_eq!(query.args(), ["-J", "-b", "-p", "--output", "NAME,SIZE"]);
        assert_eq!(
            Lsblk::new("NAME").args(),
            ["-J", "-b", "--output", "NAME"],
            "-p only when asked"
        );
    }
}

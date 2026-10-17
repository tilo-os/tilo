//! Where a block device hangs in the hardware, read from sysfs.
//!
//! A remembered "Open" for an internal drive rests on this, not on anything
//! the disk says about itself: a USB stick can copy every serial number and
//! GUID of the drive it imitates, but it cannot move itself onto the
//! motherboard's SATA controller or an `NVMe` root port.
//!
//! Every path is read under a sysfs root the caller passes, so tests build
//! the trees they need.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The storage controller nearest to a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    /// An AHCI SATA controller.
    Ahci,
    /// An `NVMe` controller attached directly to a `PCIe` root port.
    NvmeRootPort,
    /// An `NVMe` controller behind a switch or bridge: a Thunderbolt or USB4
    /// enclosure looks like this, so it never counts as internal.
    NvmeBehindBridge,
}

/// A link in a device's ancestry that can be unplugged while running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Usb,
    Thunderbolt,
    SdMmc,
    FireWire,
    /// A PCI device the kernel marks removable: everything below an
    /// external-facing port, which is how tunnelled Thunderbolt and USB4
    /// `PCIe` devices appear.
    ExternalPci,
}

/// The facts D5's location criteria are decided on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    /// The device's canonical path below the sysfs root, such as
    /// `devices/pci0000:00/0000:00:17.0/ata1/host0/.../block/sda/sda3`.
    pub device_path: String,
    /// The whole disk's kernel name (`sda`, `nvme0n1`).
    pub disk: String,
    pub controller: Option<Controller>,
    pub hotplug_link: Option<Link>,
    /// The disk's own `removable` flag.
    pub removable: bool,
    /// A device with no hardware behind it (loop, device-mapper, `NVMe` over
    /// fabrics).
    pub virtual_device: bool,
}

impl Topology {
    /// D5 criteria 1 to 3: an internal controller, no hot-pluggable link,
    /// not removable.
    pub fn is_internal(&self) -> bool {
        matches!(
            self.controller,
            Some(Controller::Ahci | Controller::NvmeRootPort)
        ) && self.hotplug_link.is_none()
            && !self.removable
            && !self.virtual_device
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn link_name(path: &Path) -> Option<String> {
    fs::read_link(path).ok().and_then(|target| {
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

fn hotplug_subsystem(subsystem: &str) -> Option<Link> {
    match subsystem {
        "usb" => Some(Link::Usb),
        "thunderbolt" => Some(Link::Thunderbolt),
        "mmc" | "mmc_host" | "sdio" | "memstick" => Some(Link::SdMmc),
        "firewire" | "ieee1394" => Some(Link::FireWire),
        _ => None,
    }
}

/// A `PCIe` host bridge directory (`pci0000:00`, or `pci10000:e0` inside an
/// Intel VMD domain), as opposed to a PCI function (`0000:00:1d.0`).
fn is_host_bridge(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("pci"))
}

/// Read the topology of block device `name` (`sdb1`, `nvme0n1p3`).
///
/// # Errors
///
/// Returns the I/O error if the device is not present under `sys_root`.
pub fn read(sys_root: &Path, name: &str) -> io::Result<Topology> {
    let device = fs::canonicalize(sys_root.join("class/block").join(name))?;
    let root = fs::canonicalize(sys_root)?;
    let relative = device
        .strip_prefix(&root)
        .map_err(|_| io::Error::other("block device outside sysfs"))?
        .to_path_buf();
    let disk_dir = if device.join("partition").exists() {
        device
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| io::Error::other("partition without a disk"))?
    } else {
        device
    };
    let disk = disk_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::other("disk without a name"))?;
    let removable = read_trimmed(&disk_dir.join("removable")).as_deref() == Some("1");
    let virtual_device = relative.starts_with("devices/virtual");

    let mut controller = None;
    let mut hotplug_link = None;
    let mut ancestor: Option<PathBuf> = disk_dir.parent().map(Path::to_path_buf);
    while let Some(dir) = ancestor {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        let subsystem = link_name(&dir.join("subsystem"));
        if hotplug_link.is_none() {
            hotplug_link = subsystem.as_deref().and_then(hotplug_subsystem);
        }
        if subsystem.as_deref() == Some("pci") {
            if hotplug_link.is_none()
                && read_trimmed(&dir.join("removable")).as_deref() == Some("removable")
            {
                hotplug_link = Some(Link::ExternalPci);
            }
            if controller.is_none() {
                controller = match link_name(&dir.join("driver")).as_deref() {
                    Some("ahci") => Some(Controller::Ahci),
                    Some("nvme") => {
                        let behind_root_port = dir
                            .parent()
                            .and_then(Path::parent)
                            .is_some_and(is_host_bridge);
                        Some(if behind_root_port {
                            Controller::NvmeRootPort
                        } else {
                            Controller::NvmeBehindBridge
                        })
                    }
                    _ => None,
                };
            }
        }
        ancestor = dir.parent().map(Path::to_path_buf);
    }

    Ok(Topology {
        device_path: relative.to_string_lossy().into_owned(),
        disk,
        controller,
        hotplug_link,
        removable,
        virtual_device,
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Fake sysfs trees shaped like real hardware.

    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    /// A sysfs tree under construction.
    pub struct Sys {
        pub root: PathBuf,
    }

    impl Sys {
        pub fn new(root: &Path) -> Self {
            for dir in [
                "class/block",
                "bus/pci",
                "bus/usb",
                "bus/thunderbolt",
                "bus/mmc",
                "dev/block",
            ] {
                fs::create_dir_all(root.join(dir)).unwrap();
            }
            for driver in ["ahci", "nvme", "pcieport", "xhci_hcd"] {
                fs::create_dir_all(root.join("bus/pci/drivers").join(driver)).unwrap();
            }
            Self {
                root: root.to_path_buf(),
            }
        }

        /// A directory at `path` belonging to `subsystem` (`pci`, `usb`...).
        pub fn device(&self, path: &str, subsystem: Option<&str>) -> PathBuf {
            let dir = self.root.join(path);
            fs::create_dir_all(&dir).unwrap();
            if let Some(subsystem) = subsystem {
                let _ = symlink(self.root.join("bus").join(subsystem), dir.join("subsystem"));
            }
            dir
        }

        pub fn pci(&self, path: &str, driver: Option<&str>, removable: bool) -> PathBuf {
            let dir = self.device(path, Some("pci"));
            if let Some(driver) = driver {
                let _ = symlink(
                    self.root.join("bus/pci/drivers").join(driver),
                    dir.join("driver"),
                );
            }
            fs::write(
                dir.join("removable"),
                if removable { "removable\n" } else { "fixed\n" },
            )
            .unwrap();
            dir
        }

        /// A disk at `path` (its directory is the disk's name) with one
        /// partition `part`, both registered under `class/block`.
        pub fn disk(&self, path: &str, part: &str, removable: bool, devs: (&str, &str)) {
            let disk = self.device(path, None);
            fs::write(
                disk.join("removable"),
                if removable { "1\n" } else { "0\n" },
            )
            .unwrap();
            fs::write(disk.join("dev"), format!("{}\n", devs.0)).unwrap();
            fs::write(disk.join("size"), "2097152\n").unwrap();
            fs::write(disk.join("diskseq"), "9\n").unwrap();
            let partition = disk.join(part);
            fs::create_dir_all(&partition).unwrap();
            fs::write(partition.join("partition"), "1\n").unwrap();
            fs::write(partition.join("dev"), format!("{}\n", devs.1)).unwrap();
            fs::write(partition.join("size"), "2048000\n").unwrap();
            let name = disk.file_name().unwrap().to_owned();
            let _ = symlink(&disk, self.root.join("class/block").join(&name));
            let _ = symlink(&partition, self.root.join("class/block").join(part));
            let _ = fs::create_dir_all(self.root.join("block"));
            let _ = symlink(&disk, self.root.join("block").join(&name));
        }

        /// An `NVMe` drive on a root port: `nvme0n1p3`.
        pub fn onboard_nvme(&self) {
            self.pci("devices/pci0000:00/0000:00:1d.0", Some("pcieport"), false);
            self.pci(
                "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0",
                Some("nvme"),
                false,
            );
            self.device("devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme", None);
            self.device(
                "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0",
                Some("nvme"),
            );
            self.disk(
                "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1",
                "nvme0n1p3",
                false,
                ("259:0", "259:3"),
            );
        }

        /// A SATA disk on the chipset's AHCI controller: `sda2`.
        pub fn onboard_sata(&self) {
            self.pci("devices/pci0000:00/0000:00:17.0", Some("ahci"), false);
            self.device("devices/pci0000:00/0000:00:17.0/ata1", None);
            self.device(
                "devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0",
                Some("scsi"),
            );
            self.disk(
                "devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda",
                "sda2",
                false,
                ("8:0", "8:2"),
            );
        }

        /// A USB stick behind the xHCI controller: `sdb1`.
        pub fn usb_stick(&self, removable: bool) {
            self.pci("devices/pci0000:00/0000:00:14.0", Some("xhci_hcd"), false);
            self.device("devices/pci0000:00/0000:00:14.0/usb2", Some("usb"));
            self.device("devices/pci0000:00/0000:00:14.0/usb2/2-1", Some("usb"));
            self.device(
                "devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0",
                Some("usb"),
            );
            self.device(
                "devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0",
                Some("scsi"),
            );
            self.disk(
                "devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb",
                "sdb1",
                removable,
                ("8:16", "8:17"),
            );
        }

        /// An `NVMe` drive in a Thunderbolt enclosure, with the kernel's
        /// removable marking below the external-facing port: `nvme1n1p1`.
        pub fn thunderbolt_nvme(&self, kernel_marks_removable: bool) {
            self.pci("devices/pci0000:00/0000:00:07.0", Some("pcieport"), false);
            self.pci(
                "devices/pci0000:00/0000:00:07.0/0000:51:00.0",
                Some("pcieport"),
                kernel_marks_removable,
            );
            self.pci(
                "devices/pci0000:00/0000:00:07.0/0000:51:00.0/0000:52:01.0",
                Some("pcieport"),
                kernel_marks_removable,
            );
            self.pci(
                "devices/pci0000:00/0000:00:07.0/0000:51:00.0/0000:52:01.0/0000:53:00.0",
                Some("nvme"),
                kernel_marks_removable,
            );
            let base =
                "devices/pci0000:00/0000:00:07.0/0000:51:00.0/0000:52:01.0/0000:53:00.0/nvme";
            self.device(base, None);
            self.device(&format!("{base}/nvme1"), Some("nvme"));
            self.disk(
                &format!("{base}/nvme1/nvme1n1"),
                "nvme1n1p1",
                false,
                ("259:4", "259:5"),
            );
        }

        /// An SD card in a PCI card reader: `mmcblk0p1`.
        pub fn sd_card(&self) {
            self.pci("devices/pci0000:00/0000:00:1c.0", Some("pcieport"), false);
            self.pci("devices/pci0000:00/0000:00:1c.0/0000:02:00.0", None, false);
            self.device(
                "devices/pci0000:00/0000:00:1c.0/0000:02:00.0/mmc_host/mmc0",
                Some("mmc_host"),
            );
            self.device(
                "devices/pci0000:00/0000:00:1c.0/0000:02:00.0/mmc_host/mmc0/mmc0:aaaa",
                Some("mmc"),
            );
            self.disk(
                "devices/pci0000:00/0000:00:1c.0/0000:02:00.0/mmc_host/mmc0/mmc0:aaaa/block/mmcblk0",
                "mmcblk0p1",
                false,
                ("179:0", "179:1"),
            );
        }

        /// A loop device: `loop0p1`.
        pub fn loop_device(&self) {
            self.disk(
                "devices/virtual/block/loop0",
                "loop0p1",
                false,
                ("7:0", "259:9"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Sys;
    use super::*;

    fn topology(build: impl FnOnce(&Sys), name: &str) -> Topology {
        let dir = tempfile::tempdir().unwrap();
        let sys = Sys::new(dir.path());
        build(&sys);
        read(dir.path(), name).unwrap()
    }

    #[test]
    fn onboard_drives_are_internal() {
        let nvme = topology(Sys::onboard_nvme, "nvme0n1p3");
        assert_eq!(nvme.controller, Some(Controller::NvmeRootPort));
        assert_eq!(nvme.disk, "nvme0n1");
        assert!(nvme.device_path.ends_with("nvme0n1/nvme0n1p3"));
        assert!(nvme.is_internal());

        let sata = topology(Sys::onboard_sata, "sda2");
        assert_eq!(sata.controller, Some(Controller::Ahci));
        assert!(sata.is_internal());
    }

    #[test]
    fn hot_pluggable_links_are_never_internal() {
        let usb = topology(|sys| sys.usb_stick(false), "sdb1");
        assert_eq!(usb.hotplug_link, Some(Link::Usb));
        assert!(
            !usb.removable,
            "a USB disk that claims removable=0 is still on USB"
        );
        assert!(!usb.is_internal());

        let sd = topology(Sys::sd_card, "mmcblk0p1");
        assert_eq!(sd.hotplug_link, Some(Link::SdMmc));
        assert!(!sd.is_internal());
    }

    #[test]
    fn a_thunderbolt_enclosure_is_not_an_onboard_nvme() {
        // Even where the firmware does not mark the port external, the drive
        // sits behind bridges rather than on a root port.
        let unmarked = topology(|sys| sys.thunderbolt_nvme(false), "nvme1n1p1");
        assert_eq!(unmarked.controller, Some(Controller::NvmeBehindBridge));
        assert!(!unmarked.is_internal());

        let marked = topology(|sys| sys.thunderbolt_nvme(true), "nvme1n1p1");
        assert_eq!(marked.hotplug_link, Some(Link::ExternalPci));
        assert!(!marked.is_internal());
    }

    #[test]
    fn removable_and_virtual_devices_are_not_internal() {
        let removable = topology(
            |sys| {
                sys.onboard_sata();
                std::fs::write(
                    sys.root.join(
                        "devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda/removable",
                    ),
                    "1\n",
                )
                .unwrap();
            },
            "sda2",
        );
        assert!(removable.removable);
        assert!(!removable.is_internal());

        let looped = topology(Sys::loop_device, "loop0p1");
        assert!(looped.virtual_device);
        assert!(!looped.is_internal());
    }
}

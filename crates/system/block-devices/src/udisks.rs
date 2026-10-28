//! `UDisks2`: the object path of a block device, and (with the `udisks`
//! feature) the calls the disk tools make on it over the system bus. Polkit
//! authorises every privileged call; nothing here needs root.

use std::fmt::Write as _;

pub const SERVICE: &str = "org.freedesktop.UDisks2";
pub const IFACE_BLOCK: &str = "org.freedesktop.UDisks2.Block";
pub const IFACE_FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
pub const IFACE_DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// The `UDisks2` `block_devices` object path for a kernel device name (`sda1`,
/// `/dev/nvme0n1p2`, `dm-0`). Every byte outside `[A-Za-z0-9]` becomes
/// `_<hex>`, the `GDBus` object-path escaping `UDisks2` uses, so `dm-0` maps
/// to `dm_2d0`.
pub fn object_path(device: &str) -> String {
    let leaf = device.strip_prefix("/dev/").unwrap_or(device);
    let mut path = String::from("/org/freedesktop/UDisks2/block_devices/");
    for byte in leaf.bytes() {
        if byte.is_ascii_alphanumeric() {
            path.push(char::from(byte));
        } else {
            let _ = write!(path, "_{byte:02x}");
        }
    }
    path
}

/// Filesystems for which migration can request a mount without journal replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnlyFilesystem {
    Ext4,
    Xfs,
    Btrfs,
}

impl ReadOnlyFilesystem {
    /// Controlled mount options: no caller-provided paths or option fragments.
    pub fn options(self) -> &'static str {
        match self {
            Self::Ext4 => "ro,nodev,nosuid,noexec,noatime,noload",
            Self::Xfs => "ro,nodev,nosuid,noexec,noatime,norecovery",
            Self::Btrfs => "ro,nodev,nosuid,noexec,noatime,nologreplay,subvolid=5",
        }
    }
}

#[cfg(feature = "udisks")]
pub use client::Udisks;

#[cfg(feature = "udisks")]
mod client {
    use std::collections::HashMap;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    use super::{
        IFACE_BLOCK, IFACE_DRIVE, IFACE_FILESYSTEM, ReadOnlyFilesystem, SERVICE, object_path,
    };

    /// Empty `a{sv}` options: `UDisks2` picks its defaults (automatic mount
    /// point, default mkfs options).
    fn no_options() -> HashMap<&'static str, Value<'static>> {
        HashMap::new()
    }

    /// A system-bus connection to `UDisks2`, addressed by kernel device name.
    pub struct Udisks {
        connection: Connection,
    }

    impl Udisks {
        /// Connect to the system bus.
        pub fn system() -> zbus::Result<Self> {
            Connection::system().map(|connection| Self { connection })
        }

        /// The device's `org.freedesktop.UDisks2.Block` interface.
        ///
        /// # Errors
        ///
        /// Returns an error when the proxy cannot be created.
        pub fn block(&self, device: &str) -> zbus::Result<Proxy<'_>> {
            Proxy::new(&self.connection, SERVICE, object_path(device), IFACE_BLOCK)
        }

        /// The device's `org.freedesktop.UDisks2.Filesystem` interface.
        ///
        /// # Errors
        ///
        /// Returns an error when the proxy cannot be created.
        pub fn filesystem(&self, device: &str) -> zbus::Result<Proxy<'_>> {
            Proxy::new(
                &self.connection,
                SERVICE,
                object_path(device),
                IFACE_FILESYSTEM,
            )
        }

        /// `Filesystem.Mount`; returns where `UDisks2` mounted it.
        pub fn mount(&self, device: &str) -> zbus::Result<String> {
            self.filesystem(device)?.call("Mount", &(no_options(),))
        }

        /// Refresh kernel partition and filesystem information after imaging.
        ///
        /// # Errors
        /// Returns an error if the daemon cannot rescan the selected device.
        pub fn rescan(&self, device: &str) -> zbus::Result<()> {
            self.block(device)?.call("Rescan", &(no_options(),))
        }

        /// Mount the verified installer's FAT filesystem for encrypted setup.
        /// Permission remains with Polkit; these options never permit execution.
        pub fn mount_setup(&self, device: &str, uid: u32, gid: u32) -> zbus::Result<String> {
            let mount_options = format!("rw,nodev,nosuid,noexec,uid={uid},gid={gid},umask=0077");
            let options = HashMap::from([
                ("fstype", Value::from("vfat")),
                ("options", Value::from(mount_options.as_str())),
            ]);
            self.filesystem(device)?.call("Mount", &(options,))
        }

        /// Mount an explicitly selected migration source without journal replay.
        /// The caller must additionally attest the returned mount against the
        /// current kernel mount table and filesystem read-only flag.
        pub fn mount_read_only(
            &self,
            device: &str,
            filesystem: ReadOnlyFilesystem,
        ) -> zbus::Result<String> {
            let options = HashMap::from([("options", Value::from(filesystem.options()))]);
            self.filesystem(device)?.call("Mount", &(options,))
        }

        /// Mount an explicitly selected retained Linux filesystem for shared edits.
        /// No forced recovery or ownership change is requested.
        ///
        /// # Errors
        /// Refuses denied or unsupported controlled mount options.
        pub fn mount_shared_files(
            &self,
            device: &str,
            filesystem: ReadOnlyFilesystem,
        ) -> zbus::Result<String> {
            let controlled = match filesystem {
                ReadOnlyFilesystem::Ext4 | ReadOnlyFilesystem::Xfs => {
                    "rw,nodev,nosuid,noexec,noatime"
                }
                ReadOnlyFilesystem::Btrfs => "rw,nodev,nosuid,noexec,noatime,subvolid=5",
            };
            let options = HashMap::from([("options", Value::from(controlled))]);
            self.filesystem(device)?.call("Mount", &(options,))
        }

        /// `Filesystem.Unmount`.
        pub fn unmount(&self, device: &str) -> zbus::Result<()> {
            self.filesystem(device)?.call("Unmount", &(no_options(),))
        }

        /// Obtain a read-only descriptor through the daemon's authorization.
        pub fn open_read_only(&self, device: &str) -> zbus::Result<std::fs::File> {
            let descriptor: zbus::zvariant::OwnedFd = self
                .block(device)?
                .call("OpenDevice", &("r", no_options()))?;
            Ok(std::fs::File::from(std::os::fd::OwnedFd::from(descriptor)))
        }

        /// Format only a caller-attested new migration partition as ext4.
        /// Ownership is assigned by the daemon to its authorized caller.
        pub fn format_migration(&self, device: &str) -> zbus::Result<()> {
            let options = HashMap::from([
                ("label", Value::from("Tilo migration")),
                (
                    "mkfs-args",
                    Value::from(crate::migration_ext4::MKFS_ARGS.to_vec()),
                ),
                ("take-ownership", Value::from(true)),
                ("update-partition-type", Value::from(false)),
            ]);
            self.block(device)?.call("Format", &("ext4", options))
        }

        /// Mount encrypted migration media without execution or device access.
        pub fn mount_migration(&self, device: &str) -> zbus::Result<String> {
            let options = HashMap::from([
                ("fstype", Value::from("ext4")),
                ("options", Value::from("rw,nodev,nosuid,noexec,noatime")),
            ]);
            self.filesystem(device)?.call("Mount", &(options,))
        }

        /// `Block.Format` with `fstype` (`ext4`, `vfat`, ...).
        pub fn format(&self, device: &str, fstype: &str) -> zbus::Result<()> {
            self.block(device)?.call("Format", &(fstype, no_options()))
        }

        /// `Drive.PowerOff` on the drive `device` is on (its `Block.Drive`):
        /// the drive is detached and powered down, ready to be pulled out.
        pub fn power_off(&self, device: &str) -> zbus::Result<()> {
            let drive: zbus::zvariant::OwnedObjectPath =
                self.block(device)?.get_property("Drive")?;
            if drive.as_str() == "/" {
                return Err(zbus::Error::Failure(format!("{device} is on no drive")));
            }
            Proxy::new(&self.connection, SERVICE, drive, IFACE_DRIVE)?
                .call("PowerOff", &(no_options(),))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_mounts_disable_writes_and_journal_replay() {
        for filesystem in [
            ReadOnlyFilesystem::Ext4,
            ReadOnlyFilesystem::Xfs,
            ReadOnlyFilesystem::Btrfs,
        ] {
            let options: Vec<_> = filesystem.options().split(',').collect();
            for required in ["ro", "nodev", "nosuid", "noexec", "noatime"] {
                assert!(options.contains(&required));
            }
            assert!(!options.contains(&"rw"));
        }
        assert!(ReadOnlyFilesystem::Ext4.options().contains("noload"));
        assert!(ReadOnlyFilesystem::Xfs.options().contains("norecovery"));
        assert!(
            ReadOnlyFilesystem::Btrfs
                .options()
                .contains("nologreplay,subvolid=5")
        );
    }

    #[test]
    fn object_paths_escape_every_byte_outside_the_plain_name_set() {
        for (case, device, want) in [
            (
                "plain_names_pass_through",
                "nvme0n1p2",
                "/org/freedesktop/UDisks2/block_devices/nvme0n1p2",
            ),
            (
                "plain_names_pass_through",
                "/dev/sda1",
                "/org/freedesktop/UDisks2/block_devices/sda1",
            ),
            (
                "other_bytes_use_gdbus_escaping",
                "dm-0",
                "/org/freedesktop/UDisks2/block_devices/dm_2d0",
            ),
            (
                "other_bytes_use_gdbus_escaping",
                "/dev/md/root_0",
                "/org/freedesktop/UDisks2/block_devices/md_2froot_5f0",
            ),
            // Path encoding cannot escape the block device namespace.
            (
                "path_encoding_cannot_escape_the_block_device_namespace",
                "../../sda",
                "/org/freedesktop/UDisks2/block_devices/_2e_2e_2f_2e_2e_2fsda",
            ),
        ] {
            assert_eq!(object_path(device), want, "case {case}: {device}");
        }
    }
}

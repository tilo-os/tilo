//! `UDisks2`: the object path of a block device, and (with the `udisks`
//! feature) the calls the disk tools make on it over the system bus. Polkit
//! authorises every privileged call; nothing here needs root.

use std::fmt::Write as _;

pub const SERVICE: &str = "org.freedesktop.UDisks2";
pub const IFACE_BLOCK: &str = "org.freedesktop.UDisks2.Block";
pub const IFACE_FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";

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

#[cfg(feature = "udisks")]
pub use client::Udisks;

#[cfg(feature = "udisks")]
mod client {
    use std::collections::HashMap;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    use super::{IFACE_BLOCK, IFACE_FILESYSTEM, SERVICE, object_path};

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
        ///
        /// # Errors
        ///
        /// Returns an error when the system bus is unreachable.
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
        ///
        /// # Errors
        ///
        /// Returns an error when the call fails or is not authorised.
        pub fn mount(&self, device: &str) -> zbus::Result<String> {
            self.filesystem(device)?.call("Mount", &(no_options(),))
        }

        /// `Filesystem.Unmount`.
        ///
        /// # Errors
        ///
        /// Returns an error when the call fails or is not authorised.
        pub fn unmount(&self, device: &str) -> zbus::Result<()> {
            self.filesystem(device)?.call("Unmount", &(no_options(),))
        }

        /// `Block.Format` with `fstype` (`ext4`, `vfat`, ...).
        ///
        /// # Errors
        ///
        /// Returns an error when the call fails or is not authorised.
        pub fn format(&self, device: &str, fstype: &str) -> zbus::Result<()> {
            self.block(device)?.call("Format", &(fstype, no_options()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_paths_escape_every_byte_outside_the_plain_name_set() {
        for (case, device, want) in [
            // Plain names pass through.
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
            // Other bytes use gdbus escaping.
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

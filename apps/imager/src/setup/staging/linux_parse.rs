//! Compatibility facade for the native staging backend.

pub use imager_backend::linux_parse::{
    LsblkDisk, MountSource, PowerSupply, backing_disks, disk_from_lsblk, disk_from_sfdisk,
    efivar_file, efivar_payload, lsblk_disk, mount_source, node_number, on_low_battery,
};

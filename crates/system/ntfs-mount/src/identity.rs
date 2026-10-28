//! What a drive says it is: its serial or WWN, its partition's GUID and the
//! NTFS volume serial.
//!
//! All of it can be forged. Remembered consent uses it only to notice that a
//! different drive now sits where a remembered one was; the location checks
//! in [`crate::topology`] carry the security.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The identifiers a remembered drive must still show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The drive's WWN, or its serial number when it reports no WWN.
    pub drive: Option<String>,
    /// The partition's GUID (or its MBR disk signature and number).
    pub partition: Option<String>,
    /// The NTFS volume serial number from the boot sector.
    pub volume: Option<u64>,
}

impl Identity {
    /// Whether every identifier is known: a remembered "Open" needs all
    /// three, or a swapped drive could match on whichever is missing.
    pub fn is_complete(&self) -> bool {
        self.drive.is_some() && self.partition.is_some() && self.volume.is_some()
    }

    /// Whether `self` shows at least one identifier and `other` shows
    /// exactly the same ones.
    pub fn matches(&self, other: &Self) -> bool {
        let any = self.drive.is_some() || self.partition.is_some() || self.volume.is_some();
        any && self == other
    }
}

/// The properties udev recorded for a block device (`/run/udev/data/bM:m`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UdevProperties(BTreeMap<String, String>);

impl UdevProperties {
    /// Parse udev's database format: one `E:KEY=VALUE` line per property.
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|line| line.strip_prefix("E:"))
                .filter_map(|entry| entry.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        )
    }

    /// Read the record for device `major:minor` under `udev_data`.
    pub fn read(udev_data: &Path, major_minor: &str) -> io::Result<Self> {
        fs::read_to_string(udev_data.join(format!("b{major_minor}"))).map(|text| Self::parse(&text))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The file system type blkid found (`ntfs`, `BitLocker`).
    pub fn fs_type(&self) -> Option<&str> {
        self.get("ID_FS_TYPE")
    }

    pub fn label(&self) -> Option<&str> {
        self.get("ID_FS_LABEL")
    }

    pub fn partition(&self) -> Option<String> {
        self.get("ID_PART_ENTRY_UUID").map(str::to_ascii_lowercase)
    }

    /// The drive's WWN, or its serial number, from udev.
    pub fn drive(&self) -> Option<String> {
        self.get("ID_WWN_WITH_EXTENSION")
            .or_else(|| self.get("ID_WWN"))
            .map(|wwn| format!("wwn:{}", wwn.to_ascii_lowercase()))
            .or_else(|| {
                self.get("ID_SERIAL_SHORT")
                    .or_else(|| self.get("ID_SERIAL"))
                    .map(|serial| format!("serial:{serial}"))
            })
    }
}

/// The NTFS boot sector facts the service reads itself: the OEM signature and
/// the volume serial, at fixed offsets. Nothing here follows a pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootSector {
    pub volume_serial: u64,
}

impl BootSector {
    /// Read the first sector's NTFS signature and serial.
    ///
    /// Returns `None` for anything that is not an NTFS boot sector.
    pub fn parse(sector: &[u8]) -> Option<Self> {
        let oem = sector.get(3..11)?;
        let signature = sector.get(510..512)?;
        if oem != b"NTFS    " || signature != [0x55, 0xAA] {
            return None;
        }
        let serial: [u8; 8] = sector.get(0x48..0x50)?.try_into().ok()?;
        Some(Self {
            volume_serial: u64::from_le_bytes(serial),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udev_records_name_the_drive_by_wwn_before_serial() {
        let props = UdevProperties::parse(
            "S:disk/by-id/x\nE:ID_FS_TYPE=ntfs\nE:ID_FS_LABEL=Windows\nE:ID_SERIAL=Samsung_SSD_970_S4EWNX0N\n\
             E:ID_WWN=eui.0025385B91B0A1C2\nE:ID_PART_ENTRY_UUID=6A1F0C2E-3B1D-4F5A-9E21-0C8F2D7A1B3C\nG:systemd\n",
        );
        assert_eq!(props.fs_type(), Some("ntfs"));
        assert_eq!(props.label(), Some("Windows"));
        assert_eq!(props.drive().as_deref(), Some("wwn:eui.0025385b91b0a1c2"));
        assert_eq!(
            props.partition().as_deref(),
            Some("6a1f0c2e-3b1d-4f5a-9e21-0c8f2d7a1b3c")
        );
        let usb = UdevProperties::parse("E:ID_SERIAL=SanDisk_Ultra_4C53\nE:ID_SERIAL_SHORT=4C53\n");
        assert_eq!(usb.drive().as_deref(), Some("serial:4C53"));
    }

    #[test]
    fn only_an_ntfs_boot_sector_yields_a_serial() {
        let mut sector = vec![0_u8; 512];
        sector[3..11].copy_from_slice(b"NTFS    ");
        sector[0x48..0x50].copy_from_slice(&0x1C8A_0E5B_8A0E_31F0_u64.to_le_bytes());
        sector[510] = 0x55;
        sector[511] = 0xAA;
        assert_eq!(
            BootSector::parse(&sector).map(|boot| boot.volume_serial),
            Some(0x1C8A_0E5B_8A0E_31F0)
        );
        sector[3..11].copy_from_slice(b"EXFAT   ");
        assert_eq!(BootSector::parse(&sector), None);
        assert_eq!(BootSector::parse(&sector[..100]), None);
    }

    #[test]
    fn identities_match_only_on_every_identifier() {
        let recorded = Identity {
            drive: Some("wwn:a".into()),
            partition: Some("p".into()),
            volume: Some(7),
        };
        assert!(recorded.is_complete());
        assert!(recorded.matches(&recorded.clone()));
        let swapped = Identity {
            volume: Some(8),
            ..recorded.clone()
        };
        assert!(!recorded.matches(&swapped));
        let unknown = Identity::default();
        assert!(!unknown.matches(&unknown), "nothing known matches nothing");
    }
}

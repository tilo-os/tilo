//! Fixture tests for the Windows adapter's pure layer. They run on every
//! development host; the byte fixtures are built with the same 64-bit layout
//! `sys.rs` asserts against `windows-sys` at compile time.

use serde_json::{Value, json};

use super::super::platform::{Disk, NewPartition, Partition, SystemVolume};
use super::super::probe::Blocker;
use super::super::{ESP_TYPE, STAGING_ROOT_TYPE};
use super::facts::{self, Observed, PowerStatus};
use super::layout::{self, Geometry};
use super::storage::{self, BitlockerState, PartitionStyle};
use super::{ops, remedies, scripts};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

#[cfg(windows)]
#[test]
fn final_boot_geometry_snapshot_executes_without_shrink_probe() {
    let fixture = Fixture::oem(512);
    let json = fixture.snapshot_json("GPT", json!("NVMe"), json!(2));
    let mocks = format!(
        r#"$fixture = '{}' | ConvertFrom-Json
$script:shrinkCalls = 0
function Get-Partition {{
 param($DriveLetter, $DiskNumber)
 if ($DriveLetter) {{ $p = $fixture.Partitions[2]; $p | Add-Member -NotePropertyName DiskNumber -NotePropertyValue 0; return $p }}
 return $fixture.Partitions
}}
function Get-Disk {{ param($Number) return $fixture.Disk }}
function Get-PartitionSupportedSize {{ $script:shrinkCalls++; return $fixture.System }}
function New-Object {{ throw 'COM intentionally unavailable in fixture' }}
"#,
        json.to_string().replace('\'', "''")
    );
    let drive = scripts::DriveLetter::new('c').unwrap();
    for (program, calls) in [
        (scripts::geometry_snapshot(drive), 0),
        (scripts::snapshot(drive), 1),
    ] {
        let program = format!(
            "{mocks}{program}\nif ($script:shrinkCalls -ne {calls}) {{ throw 'unexpected shrink probe' }}\n"
        );
        let output = super::sys::powershell(&program, std::time::Duration::from_secs(20)).unwrap();
        let value = serde_json::from_slice(&output).unwrap();
        let snapshot = storage::parse_snapshot(&value).unwrap();
        storage::cross_check(&fixture.layout_disk(), &snapshot).unwrap();
        if calls == 0 {
            assert_eq!(snapshot.system.size_min, None);
            assert_eq!(snapshot.system.size_max, None);
        }
    }
}

const MSR_TYPE: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
const BASIC_DATA_TYPE: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const WINRE_TYPE: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";
const LDM_METADATA_TYPE: &str = "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3";
const LDM_DATA_TYPE: &str = "af9b60a0-1431-4f62-bc68-3311714a69ad";

const DISK_GUID: &str = "5f1c2b3a-9d8e-4f70-a1b2-c3d4e5f60718";
const ESP_GUID: &str = "0a1b2c3d-4e5f-4a6b-8c7d-8e9f0a1b2c3d";
const MSR_GUID: &str = "1b2c3d4e-5f60-4b7c-9d8e-9fa0b1c2d3e4";
const C_GUID: &str = "2c3d4e5f-6071-4c8d-ae9f-a0b1c2d3e4f5";
const WINRE_GUID: &str = "3d4e5f60-7182-4d9e-bfa0-b1c2d3e4f506";

/// GUID text → the little-endian in-memory / on-disk encoding.
fn guid_le(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let raw: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
    out[4..6].copy_from_slice(&[raw[5], raw[4]]);
    out[6..8].copy_from_slice(&[raw[7], raw[6]]);
    out[8..16].copy_from_slice(&raw[8..16]);
    out
}

#[derive(Clone)]
struct Part {
    number: u32,
    type_guid: &'static str,
    guid: &'static str,
    offset: u64,
    len: u64,
    letter: Option<char>,
    is_boot: bool,
    is_system: bool,
}

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A `DRIVE_LAYOUT_INFORMATION_EX` output buffer.
fn layout_buf(style: u32, usable_offset: u64, usable_len: u64, parts: &[Part]) -> Vec<u8> {
    let mut buf = vec![0u8; layout::LAYOUT_HEADER_BYTES + parts.len() * layout::ENTRY_BYTES];
    put(&mut buf, 0, &style.to_le_bytes());
    put(
        &mut buf,
        4,
        &u32::try_from(parts.len()).unwrap().to_le_bytes(),
    );
    if style == layout::STYLE_GPT {
        put(&mut buf, layout::LAYOUT_GPT_DISK_ID, &guid_le(DISK_GUID));
        put(
            &mut buf,
            layout::LAYOUT_GPT_STARTING_USABLE,
            &usable_offset.to_le_bytes(),
        );
        put(
            &mut buf,
            layout::LAYOUT_GPT_USABLE_LENGTH,
            &usable_len.to_le_bytes(),
        );
    }
    for (i, p) in parts.iter().enumerate() {
        let base = layout::LAYOUT_HEADER_BYTES + i * layout::ENTRY_BYTES;
        put(&mut buf, base + layout::ENTRY_STYLE, &style.to_le_bytes());
        put(
            &mut buf,
            base + layout::ENTRY_OFFSET,
            &p.offset.to_le_bytes(),
        );
        put(&mut buf, base + layout::ENTRY_LENGTH, &p.len.to_le_bytes());
        put(
            &mut buf,
            base + layout::ENTRY_NUMBER,
            &p.number.to_le_bytes(),
        );
        if style == layout::STYLE_GPT {
            put(
                &mut buf,
                base + layout::ENTRY_GPT_TYPE,
                &guid_le(p.type_guid),
            );
            put(&mut buf, base + layout::ENTRY_GPT_ID, &guid_le(p.guid));
            let name: Vec<u8> = "Basic data partition"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            put(&mut buf, base + layout::ENTRY_GPT_NAME, &name);
        }
    }
    buf
}

fn geometry_buf(size: u64, sector: u32) -> Vec<u8> {
    let mut buf = vec![0u8; 40];
    put(
        &mut buf,
        layout::GEOMETRY_BYTES_PER_SECTOR,
        &sector.to_le_bytes(),
    );
    put(&mut buf, layout::GEOMETRY_DISK_SIZE, &size.to_le_bytes());
    buf
}

/// Primary GPT header sector and entry array for `parts` (entry index =
/// partition number).
fn gpt(sector: u64, total_sectors: u64, parts: &[Part]) -> (Vec<u8>, Vec<u8>) {
    let entry_sectors = (128 * 128u64).div_ceil(sector);
    let mut entries = vec![0u8; 128 * 128];
    for p in parts {
        let at = (p.number as usize - 1) * 128;
        put(&mut entries, at, &guid_le(p.type_guid));
        put(&mut entries, at + 16, &guid_le(p.guid));
        put(&mut entries, at + 32, &(p.offset / sector).to_le_bytes());
        put(
            &mut entries,
            at + 40,
            &((p.offset + p.len) / sector - 1).to_le_bytes(),
        );
    }
    let mut header = vec![0u8; usize::try_from(sector).unwrap()];
    put(&mut header, 0, b"EFI PART");
    put(&mut header, 8, &0x0001_0000u32.to_le_bytes());
    put(&mut header, 12, &92u32.to_le_bytes());
    put(&mut header, 24, &1u64.to_le_bytes());
    put(&mut header, 32, &(total_sectors - 1).to_le_bytes());
    put(&mut header, 40, &(2 + entry_sectors).to_le_bytes());
    put(
        &mut header,
        48,
        &(total_sectors - 2 - entry_sectors).to_le_bytes(),
    );
    put(&mut header, 56, &guid_le(DISK_GUID));
    put(&mut header, 72, &2u64.to_le_bytes());
    put(&mut header, 80, &128u32.to_le_bytes());
    put(&mut header, 84, &128u32.to_le_bytes());
    put(&mut header, 88, &crc32fast::hash(&entries).to_le_bytes());
    let crc = crc32fast::hash(&header[..92]);
    put(&mut header, 16, &crc.to_le_bytes());
    let padded = entries.len().div_ceil(sector as usize) * sector as usize;
    entries.resize(padded, 0);
    (header, entries)
}

/// A typical OEM Windows 11 layout on a 512-byte-sector NVMe disk:
/// ESP, MSR, C:, WinRE at the end.
struct Fixture {
    sector: u64,
    total_sectors: u64,
    parts: Vec<Part>,
}

impl Fixture {
    fn oem(sector: u64) -> Self {
        let size = 512 * GIB;
        let total_sectors = size / sector;
        let esp = if sector == 4096 { 260 * MIB } else { 100 * MIB };
        let entry_sectors = (128 * 128u64).div_ceil(sector);
        let last_usable_byte = (total_sectors - 2 - entry_sectors + 1) * sector;
        let winre_len = 750 * MIB;
        let winre_off = (last_usable_byte - winre_len) / MIB * MIB;
        let c_off = MIB + esp + 16 * MIB;
        let part = |number, type_guid, guid, offset, len| Part {
            number,
            type_guid,
            guid,
            offset,
            len,
            letter: None,
            is_boot: false,
            is_system: false,
        };
        let mut parts = vec![
            part(1, ESP_TYPE, ESP_GUID, MIB, esp),
            part(2, MSR_TYPE, MSR_GUID, MIB + esp, 16 * MIB),
            part(3, BASIC_DATA_TYPE, C_GUID, c_off, winre_off - c_off),
            part(4, WINRE_TYPE, WINRE_GUID, winre_off, winre_len),
        ];
        parts[0].is_system = true;
        parts[2].letter = Some('C');
        parts[2].is_boot = true;
        Self {
            sector,
            total_sectors,
            parts,
        }
    }

    fn size(&self) -> u64 {
        self.total_sectors * self.sector
    }

    fn entry_sectors(&self) -> u64 {
        (128 * 128u64).div_ceil(self.sector)
    }

    fn layout_disk(&self) -> Disk {
        let first = (2 + self.entry_sectors()) * self.sector;
        let last = (self.total_sectors - 2 - self.entry_sectors()) * self.sector;
        let buf = layout_buf(
            layout::STYLE_GPT,
            first,
            last - first + self.sector,
            &self.parts,
        );
        let layout = layout::parse_drive_layout(&buf).unwrap();
        let geometry =
            layout::parse_geometry(&geometry_buf(self.size(), self.sector as u32)).unwrap();
        layout::disk_from_layout(0, geometry, &layout).unwrap()
    }

    fn snapshot_json(&self, style: &str, bus: Value, bitlocker: Value) -> Value {
        let c = &self.parts[2];
        let partitions: Vec<Value> = self
            .parts
            .iter()
            .map(|p| {
                json!({
                    "PartitionNumber": p.number,
                    "Offset": p.offset,
                    "Size": p.len,
                    "Guid": format!("{{{}}}", p.guid.to_uppercase()),
                    "GptType": format!("{{{}}}", p.type_guid),
                    "DriveLetter": p.letter.map_or_else(|| "\u{0}".to_string(), |c| c.to_string()),
                    "IsBoot": p.is_boot,
                    "IsSystem": p.is_system,
                })
            })
            .collect();
        json!({
            "Disk": {
                "Number": 0,
                "Guid": format!("{{{DISK_GUID}}}"),
                "Size": self.size(),
                "LogicalSectorSize": self.sector,
                "PartitionStyle": style,
                "BusType": bus,
                "IsBoot": true,
                "IsSystem": true,
                "IsClustered": false,
                "IsReadOnly": false,
                "IsOffline": false
            },
            "System": {
                "PartitionNumber": c.number,
                "Offset": c.offset,
                "Size": c.len,
                "Guid": format!("{{{}}}", c.guid),
                "SizeMin": 180 * GIB,
                "SizeMax": c.len
            },
            "BitLockerProtection": bitlocker,
            "Partitions": partitions
        })
    }

    fn observed(&self, snapshot: &Value) -> Observed {
        Observed {
            uefi: Some(true),
            secure_boot_registry: Some(0),
            hiberboot_enabled: Some(0),
            hibernate_enabled: Some(1),
            hiberfile_present: false,
            power: Some(PowerStatus {
                ac_line: 1,
                battery_flag: 1,
                percent: 80,
            }),
            snapshot: storage::parse_snapshot(snapshot),
            layout_disk: Ok(self.layout_disk()),
            volume_space: Some((self.parts[2].len, 200 * GIB)),
        }
    }
}

/// GUID, CRC32, extents, truncated layouts and firmware variables.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_binary_encodings_decode_and_reject_truncation() {
    // Phase `guids_round_trip_through_the_little_endian_encoding`.
    {
        assert_eq!(layout::guid_from_le(guid_le(ESP_TYPE)), ESP_TYPE);
        assert_eq!(
            layout::normalize_guid("{C12A7328-F81F-11D2-BA4B-00A0C93EC93B}").as_deref(),
            Some(ESP_TYPE)
        );
        for bad in [
            "",
            "{}",
            "c12a7328-f81f-11d2-ba4b-00a0c93ec93",
            "c12a7328f81f11d2ba4b00a0c93ec93b",
            "c12a7328-f81f-11d2-ba4b-00a0c93ec93g",
            "{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}'; rm",
        ] {
            assert_eq!(layout::normalize_guid(bad), None, "{bad:?}");
        }
    }

    // Phase `volume_extents_decode`.
    {
        let mut buf = vec![0u8; layout::EXTENTS_FIRST + 2 * layout::EXTENT_BYTES];
        put(&mut buf, 0, &2u32.to_le_bytes());
        put(&mut buf, 8, &3u32.to_le_bytes());
        put(&mut buf, 16, &MIB.to_le_bytes());
        put(&mut buf, 24, &GIB.to_le_bytes());
        put(&mut buf, 32, &1u32.to_le_bytes());
        let extents = layout::parse_volume_extents(&buf).unwrap();
        assert_eq!(extents.len(), 2);
        assert_eq!(extents[0].disk_number, 3);
        assert_eq!(extents[0].offset, MIB);
        assert_eq!(extents[0].length, GIB);
        assert_eq!(extents[1].disk_number, 1);
        assert!(layout::parse_volume_extents(&buf[..40]).is_err());
    }

    // Phase `truncated_or_negative_layouts_are_rejected`.
    {
        let fx = Fixture::oem(512);
        let buf = layout_buf(layout::STYLE_GPT, 34 * 512, GIB, &fx.parts);
        assert!(layout::parse_drive_layout(&buf[..buf.len() - 1]).is_err());
        let mut negative = buf;
        put(
            &mut negative,
            layout::LAYOUT_HEADER_BYTES + layout::ENTRY_OFFSET,
            &(-512i64).to_le_bytes(),
        );
        assert!(layout::parse_drive_layout(&negative).is_err());
        assert!(layout::parse_geometry(&[0u8; 8]).is_err());
    }
}

/// OEM layout, probe offer, snapshot protection and 4Kn bounds.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_layout_decodes_protects_and_aligns_the_staging_disk() {
    // Phase `oem_layout_decodes_into_the_staging_disk`.
    {
        let fx = Fixture::oem(512);
        let disk = fx.layout_disk();
        assert_eq!(disk.id, "0");
        assert_eq!(disk.guid, DISK_GUID);
        assert_eq!(disk.sector_bytes, 512);
        assert_eq!(disk.size_bytes, 512 * GIB);
        assert_eq!(disk.first_usable_lba, 34);
        assert_eq!(disk.last_usable_lba, fx.total_sectors - 34);
        let numbers: Vec<u32> = disk.partitions.iter().map(|p| p.number).collect();
        assert_eq!(numbers, [1, 2, 3, 4]);
        assert_eq!(
            disk.partitions[0],
            Partition {
                number: 1,
                guid: ESP_GUID.into(),
                type_guid: ESP_TYPE.into(),
                first_lba: 2048,
                last_lba: 2048 + 204_800 - 1,
            }
        );
        let winre = &disk.partitions[3];
        assert_eq!(winre.type_guid, WINRE_TYPE);
        assert!(winre.last_lba <= disk.last_usable_lba);

        let layout = layout::parse_drive_layout(&layout_buf(
            layout::STYLE_GPT,
            34 * 512,
            (fx.total_sectors - 67) * 512,
            &fx.parts,
        ))
        .unwrap();
        assert_eq!(layout.partitions[2].name, "Basic data partition");
    }

    // Phase `oem_layout_probe_offers_full_tilo_with_empty_boot_fields`.
    {
        let fx = Fixture::oem(512);
        let snapshot = fx.snapshot_json("GPT", json!("NVMe"), json!(2));
        let observed = fx.observed(&snapshot);
        let facts = facts::assemble(&observed);
        assert_eq!(facts.blockers, Vec::<Blocker>::new());
        assert!(!facts.bitlocker_protected);
        assert!(facts.offers_full_os());
        let basis = facts.basis.unwrap();
        assert_eq!(basis.disk, fx.layout_disk());
        assert_eq!(
            basis.system_volume,
            Some(SystemVolume {
                partition_guid: C_GUID.into(),
                size_bytes: fx.parts[2].len,
                supported_min_bytes: 180 * GIB,
                used_bytes: fx.parts[2].len - 200 * GIB,
            })
        );
        assert!(basis.firmware.uefi);
        assert!(!basis.firmware.secure_boot);
        assert_eq!(basis.firmware.boot_current, None);
        assert!(basis.firmware.boot_order.is_empty());
        assert_eq!(basis.firmware.boot_next, None);
        assert_eq!(facts::basis_error(&observed), None);
    }

    // Phase `snapshot_protects_boot_system_and_lettered_partitions`.
    {
        let fx = Fixture::oem(512);
        let snapshot =
            storage::parse_snapshot(&fx.snapshot_json("GPT", json!("NVMe"), json!(2))).unwrap();
        assert_eq!(
            snapshot.partitions[0].drive_letter, None,
            "NUL is no letter"
        );
        assert_eq!(snapshot.partitions[2].drive_letter, Some('C'));
        let protected = storage::protected_partitions(&snapshot);
        assert!(protected.contains(&ESP_GUID.to_string()));
        assert!(protected.contains(&C_GUID.to_string()));
        assert!(!protected.contains(&WINRE_GUID.to_string()));
    }

    // Phase `four_k_native_disk_uses_six_sector_gpt_bounds`.
    {
        let fx = Fixture::oem(4096);
        let disk = fx.layout_disk();
        assert_eq!(disk.sector_bytes, 4096);
        assert_eq!(disk.first_usable_lba, 6);
        assert_eq!(disk.last_usable_lba, fx.total_sectors - 6);
        assert_eq!(disk.partitions[0].first_lba, 256);
        let snapshot =
            storage::parse_snapshot(&fx.snapshot_json("GPT", json!(17), json!(null))).unwrap();
        // The cmdlet-derived fallback agrees with the header-derived bounds.
        let fallback = storage::disk_from_snapshot(&snapshot).unwrap();
        assert_eq!(fallback, disk);
        storage::cross_check(&disk, &snapshot).unwrap();
        // GPT on-disk verification (elevated helper path).
        let (header, entries) = gpt(4096, fx.total_sectors, &fx.parts);
        let header = layout::parse_gpt_header(&header).unwrap();
        assert_eq!(header.entries_read_bytes(4096).unwrap(), 16 * 1024);
        let entries = layout::parse_gpt_entries(&entries, &header).unwrap();
        layout::cross_check_gpt(&disk, &header, &entries).unwrap();
    }

    // Phase `a_misaligned_partition_on_a_4kn_disk_is_refused`.
    {
        let mut fx = Fixture::oem(4096);
        fx.parts[3].offset += 512;
        let first = 6 * 4096;
        let len = (fx.total_sectors - 11) * 4096;
        let layout =
            layout::parse_drive_layout(&layout_buf(layout::STYLE_GPT, first, len, &fx.parts))
                .unwrap();
        let geometry = Geometry {
            disk_size: fx.size(),
            bytes_per_sector: 4096,
        };
        assert!(layout::disk_from_layout(0, geometry, &layout).is_err());
    }
}

/// MBR, dynamic, RAID/spaces/removable/VHD disks.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_unsupported_disks_leave_no_basis() {
    // Phase `mbr_disk_is_not_gpt_without_a_basis`.
    {
        let fx = Fixture::oem(512);
        let mut snapshot = fx.snapshot_json("MBR", json!("SATA"), json!(2));
        snapshot["Disk"]["Guid"] = json!("");
        for p in snapshot["Partitions"].as_array_mut().unwrap() {
            p["Guid"] = json!("");
            p["GptType"] = json!("");
        }
        let mut observed = fx.observed(&snapshot);
        let mbr_layout =
            layout::parse_drive_layout(&layout_buf(layout::STYLE_MBR, 0, 0, &fx.parts)).unwrap();
        assert_eq!(mbr_layout.disk_guid, None);
        observed.layout_disk = layout::disk_from_layout(
            0,
            Geometry {
                disk_size: fx.size(),
                bytes_per_sector: 512,
            },
            &mbr_layout,
        );
        assert!(observed.layout_disk.is_err());
        let facts = facts::assemble(&observed);
        assert_eq!(facts.blockers, vec![Blocker::NotGpt]);
        assert!(facts.basis.is_none());
        assert!(!facts.offers_full_os());
        assert_eq!(
            storage::parse_snapshot(&snapshot).unwrap().disk.style,
            PartitionStyle::Mbr
        );
    }

    // Phase `dynamic_disk_is_unsupported`.
    {
        let mut fx = Fixture::oem(512);
        fx.parts[2].type_guid = LDM_DATA_TYPE;
        fx.parts[3].type_guid = LDM_METADATA_TYPE;
        let snapshot = fx.snapshot_json("GPT", json!("NVMe"), json!(2));
        let parsed = storage::parse_snapshot(&snapshot).unwrap();
        assert_eq!(
            storage::unsupported_reason(&parsed).as_deref(),
            Some("the system disk is a dynamic disk")
        );
        let facts = facts::assemble(&fx.observed(&snapshot));
        assert_eq!(facts.blockers, vec![Blocker::DiskUnsupported]);
        assert!(!facts.offers_full_os());
    }

    // Phase `raid_spaces_removable_and_vhd_boot_disks_are_unsupported`.
    {
        let fx = Fixture::oem(512);
        for bus in [
            json!("RAID"),
            json!("Storage Spaces"),
            json!("Spaces"),
            json!("USB"),
            json!("SD"),
            json!("File Backed Virtual"),
            json!(8),
            json!(16),
            json!("iSCSI"),
            json!("Unknown"),
            json!(null),
        ] {
            let snapshot = fx.snapshot_json("GPT", bus.clone(), json!(2));
            let facts = facts::assemble(&fx.observed(&snapshot));
            assert!(
                facts.blockers.contains(&Blocker::DiskUnsupported),
                "{bus} must be unsupported"
            );
        }
        for bus in [
            json!("NVMe"),
            json!("SATA"),
            json!("SAS"),
            json!("SCSI"),
            json!(17),
            json!(11),
        ] {
            let snapshot = fx.snapshot_json("GPT", bus.clone(), json!(2));
            assert!(
                facts::assemble(&fx.observed(&snapshot)).blockers.is_empty(),
                "{bus}"
            );
        }
    }
}

/// BitLocker, disagreeing readers, missing bounds, ioctl fallback, firmware and remedies.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_blockers_and_remedies_follow_reader_facts() {
    // Phase `bitlocker_states_map_to_protection_and_blockers`.
    {
        let fx = Fixture::oem(512);
        let run = |value: Value| {
            facts::assemble(&fx.observed(&fx.snapshot_json("GPT", json!("NVMe"), value)))
        };
        let on = run(json!(1));
        assert!(on.bitlocker_protected);
        assert!(on.blockers.is_empty(), "protection alone does not block");
        assert!(on.offers_full_os());
        let unknown = run(json!(null));
        assert!(
            unknown.bitlocker_protected,
            "unknown fails closed as protected"
        );
        assert!(unknown.blockers.is_empty());
        for converting in [3, 4, 6] {
            let facts = run(json!(converting));
            assert_eq!(
                facts.blockers,
                vec![Blocker::BitlockerLocked],
                "{converting}"
            );
            assert!(!facts.offers_full_os());
        }
        for off in [0, 2, 5, 8] {
            assert!(!run(json!(off)).bitlocker_protected, "{off}");
        }
        assert_eq!(storage::bitlocker_state(Some(99)), BitlockerState::Unknown);
    }

    // Phase `disagreeing_readers_leave_no_basis`.
    {
        let fx = Fixture::oem(512);
        let mut snapshot = fx.snapshot_json("GPT", json!("NVMe"), json!(2));
        snapshot["Partitions"][3]["Size"] = json!(fx.parts[3].len - MIB);
        let observed = fx.observed(&snapshot);
        let facts = facts::assemble(&observed);
        assert!(facts.basis.is_none());
        assert_eq!(facts.blockers, vec![Blocker::DiskUnsupported]);
        assert!(facts::basis_error(&observed).unwrap().contains("disagree"));
    }

    // Phase `missing_shrink_bounds_or_snapshot_leave_no_basis`.
    {
        let fx = Fixture::oem(512);
        let mut snapshot = fx.snapshot_json("GPT", json!("NVMe"), json!(2));
        snapshot["System"]["SizeMin"] = Value::Null;
        let facts = facts::assemble(&fx.observed(&snapshot));
        assert!(facts.basis.is_none());
        assert_eq!(facts.blockers, vec![Blocker::DiskUnsupported]);

        let mut observed = fx.observed(&fx.snapshot_json("GPT", json!("NVMe"), json!(2)));
        observed.snapshot = Err("PowerShell failed".into());
        let facts = facts::assemble(&observed);
        assert!(facts.basis.is_none());
        assert!(facts.bitlocker_protected);
        assert_eq!(facts.blockers, vec![Blocker::DiskUnsupported]);
    }

    // Phase `layout_ioctl_failure_falls_back_to_the_cmdlet_disk`.
    {
        let fx = Fixture::oem(512);
        let mut observed = fx.observed(&fx.snapshot_json("GPT", json!("NVMe"), json!(2)));
        observed.layout_disk = Err("access denied".into());
        let facts = facts::assemble(&observed);
        assert_eq!(facts.basis.unwrap().disk, fx.layout_disk());
    }

    // Phase `firmware_power_and_hibernation_blockers`.
    {
        let fx = Fixture::oem(512);
        let snapshot = fx.snapshot_json("GPT", json!("NVMe"), json!(2));
        let mut observed = fx.observed(&snapshot);
        observed.uefi = Some(false);
        let facts = facts::assemble(&observed);
        assert_eq!(facts.blockers, vec![Blocker::NotUefi]);
        assert!(!facts.basis.unwrap().firmware.uefi);

        let mut observed = fx.observed(&snapshot);
        observed.secure_boot_registry = None;
        observed.hiberboot_enabled = Some(1);
        observed.power = Some(PowerStatus {
            ac_line: 0,
            battery_flag: 1,
            percent: 49,
        });
        let facts = facts::assemble(&observed);
        assert_eq!(
            facts.blockers,
            vec![
                Blocker::SecureBootOn,
                Blocker::Hibernation,
                Blocker::OnBattery
            ]
        );
        assert!(facts.offers_full_os(), "every one of these has a remedy");
        assert!(facts.basis.unwrap().firmware.secure_boot);

        assert!(facts::secure_boot_enabled(Some(1)));
        assert!(!facts::secure_boot_enabled(Some(0)));
        assert!(facts::hibernation_active(Some(0), Some(0), true));
        assert!(facts::hibernation_active(None, None, false));
        assert!(!facts::hibernation_active(Some(1), Some(0), false));
        assert!(!facts::hibernation_active(Some(0), None, false));
        let status = |ac_line, battery_flag, percent| {
            Some(PowerStatus {
                ac_line,
                battery_flag,
                percent,
            })
        };
        assert!(!facts::on_battery(status(0, 1, 50)));
        assert!(facts::on_battery(status(0, 1, 255)));
        assert!(!facts::on_battery(status(255, 128, 255)));
        assert!(facts::on_battery(status(255, 8, 20)));
        assert!(facts::on_battery(None));
    }

    // Phase `remedies_suspend_bitlocker_before_firmware_restart`.
    {
        let drive = scripts::DriveLetter::new('C').unwrap();
        let program = remedies::firmware_settings_program(drive, true);
        let suspend = program.find("DisableKeyProtectors").unwrap();
        let restart = program.find("shutdown.exe\" /r /fw /t 0").unwrap();
        assert!(suspend < restart);
        assert!(program.contains("DisableCount = [uint32]1"));
        assert!(program.contains("DriveLetter='C:'"));
        assert!(program.contains("$protectedHint = $true"));
        assert!(
            remedies::firmware_settings_program(drive, false).contains("$protectedHint = $false")
        );
        assert!(remedies::disable_fast_startup_program().contains("powercfg.exe\" /h off"));
        assert!(remedies::describe_exit(Some(0)).is_ok());
        assert!(
            remedies::describe_exit(Some(1223))
                .unwrap_err()
                .contains("not granted")
        );
        assert!(
            remedies::describe_exit(Some(22))
                .unwrap_err()
                .contains("BitLocker")
        );
        assert!(remedies::describe_exit(None).is_err());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_disk_io_verifies_gpt_preconditions_and_ranges() {
    // Phase `gpt_verification_catches_corruption_and_renumbering`.
    {
        let fx = Fixture::oem(512);
        let disk = fx.layout_disk();
        let (header, entries) = gpt(512, fx.total_sectors, &fx.parts);
        let parsed = layout::parse_gpt_header(&header).unwrap();
        assert_eq!(parsed.first_usable_lba, 34);
        assert_eq!(parsed.disk_guid, DISK_GUID);
        let list = layout::parse_gpt_entries(&entries, &parsed).unwrap();
        assert_eq!(list.len(), 4);
        layout::cross_check_gpt(&disk, &parsed, &list).unwrap();

        let mut bad = header;
        bad[40] ^= 1;
        assert!(
            layout::parse_gpt_header(&bad)
                .unwrap_err()
                .contains("checksum")
        );
        let mut bad = entries;
        bad[33] ^= 1;
        assert!(
            layout::parse_gpt_entries(&bad, &parsed)
                .unwrap_err()
                .contains("checksum")
        );
        assert!(layout::parse_gpt_header(&[0u8; 512]).is_err());

        // WinRE sits in GPT slot 5 while Windows calls it partition 4.
        let mut moved = fx.parts.clone();
        moved[3].number = 5;
        let (header, entries) = gpt(512, fx.total_sectors, &moved);
        let parsed = layout::parse_gpt_header(&header).unwrap();
        let list = layout::parse_gpt_entries(&entries, &parsed).unwrap();
        let error = layout::cross_check_gpt(&disk, &parsed, &list).unwrap_err();
        assert!(error.contains("GPT entry 5"), "{error}");
    }

    // Phase `create_preconditions_and_read_back`.
    {
        let fx = Fixture::oem(512);
        let mut disk = fx.layout_disk();
        // Shrunk C: leaves room before WinRE.
        let c_last = disk.partitions[2].first_lba + 200 * GIB / 512 - 1;
        disk.partitions[2].last_lba = c_last;
        let esp = NewPartition {
            first_lba: c_last + 1 + 2048 + 64 * GIB / 512,
            last_lba: c_last + 2048 + 64 * GIB / 512 + 2 * GIB / 512,
            type_guid: ESP_TYPE.into(),
            name: "Tilo installer boot".into(),
            guid: None,
        };
        let (offset, size) = ops::create_extent(&disk, &esp).unwrap();
        assert_eq!(offset, esp.first_lba * 512);
        assert_eq!(size, 2 * GIB);

        let mut with_guid = esp.clone();
        with_guid.guid = Some(ESP_GUID.into());
        assert!(ops::create_extent(&disk, &with_guid).is_err());
        let mut data = esp.clone();
        data.type_guid = BASIC_DATA_TYPE.into();
        assert!(ops::create_extent(&disk, &data).is_err());
        let mut overlap = esp.clone();
        overlap.first_lba = c_last;
        assert!(ops::create_extent(&disk, &overlap).is_err());
        let mut past_end = esp;
        past_end.first_lba = disk.last_usable_lba;
        past_end.last_lba = disk.last_usable_lba + 1;
        assert!(ops::create_extent(&disk, &past_end).is_err());

        let good = json!({"PartitionNumber": 5, "Offset": offset, "Size": size,
            "Guid": "{9E8D7C6B-5A49-4382-9170-6F5E4D3C2B1A}", "GptType": "{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}",
            "DriveLetter": "\u{0}", "AccessPaths": ["\\\\?\\Volume{9e8d7c6b-5a49-4382-9170-6f5e4d3c2b1a}\\"]});
        assert_eq!(
            ops::parse_created(&good, offset, size, ESP_TYPE).unwrap(),
            "9e8d7c6b-5a49-4382-9170-6f5e4d3c2b1a"
        );
        let mut lettered = good.clone();
        lettered["DriveLetter"] = json!("E");
        assert!(ops::parse_created(&lettered, offset, size, ESP_TYPE).is_err());
        let mut mounted = good.clone();
        mounted["AccessPaths"] = json!(["E:\\"]);
        assert!(ops::parse_created(&mounted, offset, size, ESP_TYPE).is_err());
        assert!(ops::parse_created(&good, offset + MIB, size, ESP_TYPE).is_err());
        assert!(ops::parse_created(&good, offset, size, STAGING_ROOT_TYPE).is_err());

        ops::parse_resized(
            &json!({"Size": 200 * GIB, "Guid": format!("{{{C_GUID}}}")}),
            C_GUID,
            200 * GIB,
        )
        .unwrap();
        assert!(
            ops::parse_resized(
                &json!({"Size": 200 * GIB + MIB, "Guid": C_GUID}),
                C_GUID,
                200 * GIB
            )
            .is_err()
        );
    }

    // Phase `raw_io_ranges_and_volume_overlap_rules`.
    {
        assert_eq!(
            ops::io_offset(MIB, GIB, 512, 4096, 8192).unwrap(),
            MIB + 4096
        );
        assert!(ops::io_offset(MIB, GIB, 512, 100, 512).is_err());
        assert!(ops::io_offset(MIB, GIB, 4096, 0, 512).is_err());
        assert!(ops::io_offset(MIB, GIB, 512, GIB - 512, 1024).is_err());
        assert_eq!(
            ops::io_offset(MIB, GIB, 512, GIB - 512, 512).unwrap(),
            MIB + GIB - 512
        );
        assert_eq!(ops::volume_extent_allowed(GIB, GIB, 0, GIB), Some(true));
        assert_eq!(ops::volume_extent_allowed(GIB, GIB, GIB, GIB), Some(true));
        assert_eq!(
            ops::volume_extent_allowed(GIB, GIB, GIB - MIB, 2 * MIB),
            Some(false)
        );
        assert_eq!(
            ops::volume_extent_allowed(GIB, GIB, 0, 3 * GIB),
            Some(false)
        );
        assert!(ops::is_staging_type(&STAGING_ROOT_TYPE.to_uppercase()));
        assert!(!ops::is_staging_type(BASIC_DATA_TYPE));
        assert!(ops::valid_efi_name("Boot0100"));
        assert!(ops::valid_efi_name("BootNext"));
        assert!(!ops::valid_efi_name("Boot-0100"));
        assert!(!ops::valid_efi_name(""));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn windows_scripts_carry_only_validated_encoded_values() {
    // Phase `scripts_interpolate_only_validated_values`.
    {
        assert_eq!(
            scripts::ps_guid(ESP_TYPE).unwrap(),
            format!("{{{ESP_TYPE}}}")
        );
        assert_eq!(scripts::ps_guid("x'; Remove-Partition"), None);
        assert!(scripts::resize(0, 3, "not-a-guid", GIB).is_none());
        assert!(scripts::create(0, MIB, GIB, "'; format c:").is_none());
        assert!(scripts::delete(0, 5, "{bad}", MIB, GIB).is_none());

        let resize = scripts::resize(0, 3, C_GUID, 300 * GIB).unwrap();
        assert!(resize.contains(&format!(
            "Resize-Partition -DiskNumber 0 -PartitionNumber 3 -Size {}",
            300 * GIB
        )));
        assert!(resize.contains(&format!("'{{{C_GUID}}}'")));
        let create = scripts::create(1, 400 * GIB, 2 * GIB, &ESP_TYPE.to_uppercase()).unwrap();
        assert!(create.contains(&format!(
            "New-Partition -DiskNumber 1 -Offset {} -Size {} -GptType '{{{ESP_TYPE}}}'",
            400 * GIB,
            2 * GIB
        )));
        assert!(!create.contains("AssignDriveLetter"));
        let delete = scripts::delete(1, 6, WINRE_GUID, MIB, GIB).unwrap();
        assert!(
            delete.contains("Remove-Partition -DiskNumber 1 -PartitionNumber 6 -Confirm:$false")
        );
        for script in [
            &resize,
            &create,
            &delete,
            &scripts::snapshot(scripts::DriveLetter::new('c').unwrap()),
        ] {
            assert!(script.starts_with("$ErrorActionPreference = 'Stop'"));
        }
        assert!(
            scripts::snapshot(scripts::DriveLetter::new('d').unwrap()).contains("-DriveLetter D\n")
        );
        let drive = scripts::DriveLetter::new('c').unwrap();
        let full = scripts::snapshot(drive);
        let geometry = scripts::geometry_snapshot(drive);
        assert!(full.contains("Get-PartitionSupportedSize"));
        assert!(!geometry.contains("Get-PartitionSupportedSize"));
        for retained in [
            "Get-Disk",
            "Get-Partition -DiskNumber",
            "Guid =",
            "LogicalSectorSize",
            "SizeMin = $min",
            "BitLockerProtection",
        ] {
            assert!(
                geometry.contains(retained),
                "missing geometry check: {retained}"
            );
        }
        assert_eq!(
            scripts::DriveLetter::of_path(r"C:\Windows")
                .unwrap()
                .letter(),
            'C'
        );
        assert_eq!(scripts::DriveLetter::of_path(r"\\server\share"), None);
        assert_eq!(scripts::DriveLetter::new('1'), None);
    }

    // Phase `powershell_programs_travel_as_encoded_commands`.
    {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(scripts::base64(plain.as_bytes()), encoded);
        }
        assert_eq!(scripts::encode_command("a"), "YQA=");
        let args = scripts::powershell_args("exit 0");
        assert_eq!(
            &args[..5],
            [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand"
            ]
        );
        let outer = scripts::elevated(r"C:\Win'dows\powershell.exe", "exit 7");
        assert!(outer.contains(r"-FilePath 'C:\Win''dows\powershell.exe' -Verb RunAs"));
        assert!(outer.contains(&scripts::encode_command("exit 7")));
        assert!(outer.contains("exit 1223"));
    }
}

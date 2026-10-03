//! `EFI_LOAD_OPTION` encoding for the owned installer boot entry, shared by
//! both platforms so one tested encoder produces the bytes firmware sees.
//!
//! Layout (UEFI 2.10 §3.1.3): `u32 Attributes`, `u16 FilePathListLength`,
//! NUL-terminated UTF-16LE `Description`, `FilePathList`, `OptionalData`.
//! The device path is `HD` (§10.3.5.1: type 4, subtype 1, length 42),
//! `File` (§10.3.5.4: type 4, subtype 4, NUL-terminated UTF-16LE path) and
//! `End Entire Device Path` (type 0x7f, subtype 0xff, length 4).

/// `EFI_GLOBAL_VARIABLE` vendor GUID.
pub const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
pub const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;
/// Owned entries are allocated at or above this number.
pub const FIRST_OWNED_BOOT_NUMBER: u16 = 0x0100;

const MEDIA_DEVICE_PATH: u8 = 0x04;
const MEDIA_HARDDRIVE_DP: u8 = 0x01;
const MEDIA_FILEPATH_DP: u8 = 0x04;
const END_DEVICE_PATH_TYPE: u8 = 0x7f;
const END_ENTIRE_DEVICE_PATH_SUBTYPE: u8 = 0xff;
const HD_NODE_LEN: usize = 42;
const MBR_TYPE_GPT: u8 = 0x02;
const SIGNATURE_TYPE_GUID: u8 = 0x02;
/// Bounds on what this encoder will produce or accept.
const MAX_TEXT_UNITS: usize = 256;

/// Inputs for an HD(...)/File(...) load option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardDriveLoadOption {
    pub description: String,
    pub partition_number: u32,
    pub start_lba: u64,
    pub size_lba: u64,
    pub partition_guid: String,
    /// Backslash-separated, e.g. `\EFI\BOOT\BOOTX64.EFI`.
    pub file_path: String,
}

/// `Boot####` variable name for a boot number.
pub fn boot_var_name(number: u16) -> String {
    format!("Boot{number:04X}")
}

/// Four uppercase hex digits, as used in `BootOrder` text and records.
pub fn format_boot_number(number: u16) -> String {
    format!("{number:04X}")
}

/// Parse four hex digits (`0004`, case-insensitive).
pub fn parse_boot_number(text: &str) -> Result<u16, String> {
    if text.len() != 4 || !text.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid boot number {text:?}"));
    }
    u16::from_str_radix(text, 16).map_err(|e| e.to_string())
}

/// `BootOrder`/`BootNext` payloads are little-endian `u16` arrays.
pub fn parse_u16_list(bytes: &[u8]) -> Result<Vec<u16>, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("odd-length boot number list".into());
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect())
}

pub fn encode_u16(number: u16) -> Vec<u8> {
    number.to_le_bytes().to_vec()
}

/// The lowest free `Boot####` at or above [`FIRST_OWNED_BOOT_NUMBER`] that is
/// neither referenced by `boot_order` nor present (`exists` asks firmware).
pub fn allocate_boot_number(
    boot_order: &[u16],
    mut exists: impl FnMut(u16) -> Result<bool, String>,
) -> Result<u16, String> {
    for number in FIRST_OWNED_BOOT_NUMBER..=0xFFFF {
        if boot_order.contains(&number) {
            continue;
        }
        if !exists(number)? {
            return Ok(number);
        }
    }
    Err("no free boot option number".into())
}

/// Mixed-endian EFI GUID bytes from canonical text.
pub fn guid_bytes(text: &str) -> Result<[u8; 16], String> {
    let lower = text.to_ascii_lowercase();
    if !super::is_guid(&lower) {
        return Err(format!("invalid GUID {text:?}"));
    }
    let nibbles: Vec<u8> = lower
        .bytes()
        .filter(|c| *c != b'-')
        .map(|c| match c {
            b'0'..=b'9' => c - b'0',
            _ => c - b'a' + 10,
        })
        .collect();
    let mut raw = [0_u8; 16];
    for (byte, pair) in raw.iter_mut().zip(nibbles.chunks_exact(2)) {
        *byte = (pair[0] << 4) | pair[1];
    }
    raw[0..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    Ok(raw)
}

/// Canonical lowercase text from mixed-endian EFI GUID bytes.
pub fn guid_text(bytes: &[u8; 16]) -> String {
    let mut raw = *bytes;
    raw[0..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    let h: Vec<String> = raw.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

fn utf16z(text: &str, what: &str) -> Result<Vec<u8>, String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() || units.len() > MAX_TEXT_UNITS || units.contains(&0) {
        return Err(format!("invalid {what}"));
    }
    Ok(units
        .iter()
        .chain(std::iter::once(&0))
        .flat_map(|u| u.to_le_bytes())
        .collect())
}

fn valid_file_path(path: &str) -> bool {
    path.starts_with('\\')
        && !path.contains("\\\\")
        && !path.ends_with('\\')
        && path
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'/' && c != b'"')
}

/// Encode an `EFI_LOAD_OPTION` (attributes ACTIVE, HD + File + End device
/// path, no optional data).
pub fn encode_load_option(option: &HardDriveLoadOption) -> Result<Vec<u8>, String> {
    if option.partition_number == 0 || option.size_lba == 0 || option.start_lba == 0 {
        return Err("invalid HD() partition fields".into());
    }
    if !valid_file_path(&option.file_path) {
        return Err(format!("invalid loader path {:?}", option.file_path));
    }
    let description = utf16z(&option.description, "description")?;
    let path = utf16z(&option.file_path, "loader path")?;
    let mut device_path = Vec::with_capacity(HD_NODE_LEN + 4 + path.len() + 4);
    device_path.extend([MEDIA_DEVICE_PATH, MEDIA_HARDDRIVE_DP]);
    device_path.extend((HD_NODE_LEN as u16).to_le_bytes());
    device_path.extend(option.partition_number.to_le_bytes());
    device_path.extend(option.start_lba.to_le_bytes());
    device_path.extend(option.size_lba.to_le_bytes());
    device_path.extend(guid_bytes(&option.partition_guid)?);
    device_path.extend([MBR_TYPE_GPT, SIGNATURE_TYPE_GUID]);
    device_path.extend([MEDIA_DEVICE_PATH, MEDIA_FILEPATH_DP]);
    device_path.extend(
        u16::try_from(4 + path.len())
            .map_err(|e| e.to_string())?
            .to_le_bytes(),
    );
    device_path.extend(&path);
    device_path.extend([END_DEVICE_PATH_TYPE, END_ENTIRE_DEVICE_PATH_SUBTYPE, 4, 0]);
    let mut out = Vec::with_capacity(6 + description.len() + device_path.len());
    out.extend(LOAD_OPTION_ACTIVE.to_le_bytes());
    out.extend(
        u16::try_from(device_path.len())
            .map_err(|e| e.to_string())?
            .to_le_bytes(),
    );
    out.extend(description);
    out.extend(device_path);
    Ok(out)
}

fn read_utf16z(bytes: &[u8]) -> Result<(String, usize), String> {
    let mut units = Vec::new();
    let mut at = 0;
    loop {
        let pair = bytes.get(at..at + 2).ok_or("unterminated UTF-16 string")?;
        at += 2;
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
        if units.len() > MAX_TEXT_UNITS {
            return Err("UTF-16 string too long".into());
        }
    }
    Ok((String::from_utf16(&units).map_err(|e| e.to_string())?, at))
}

/// Decode what [`encode_load_option`] produces (and firmware-written HD/File
/// options): an `HD` node, one or more `File` nodes (joined as firmware
/// concatenates them), then `End`. Trailing optional data is permitted and
/// ignored. Options whose device path starts elsewhere (PciRoot, URI, ...)
/// are rejected; callers compare those by bytes only.
pub fn decode_load_option(bytes: &[u8]) -> Result<HardDriveLoadOption, String> {
    let header = bytes.get(..6).ok_or("load option too short")?;
    let list_len = usize::from(u16::from_le_bytes([header[4], header[5]]));
    let (description, used) = read_utf16z(&bytes[6..])?;
    let start = 6 + used;
    let list = bytes
        .get(start..start + list_len)
        .ok_or("device path list exceeds the load option")?;
    let node = |at: usize| -> Result<(u8, u8, &[u8]), String> {
        let head = list.get(at..at + 4).ok_or("truncated device path node")?;
        let len = usize::from(u16::from_le_bytes([head[2], head[3]]));
        let body = list
            .get(at + 4..at + len)
            .filter(|_| len >= 4)
            .ok_or("device path node exceeds the list")?;
        Ok((head[0], head[1], body))
    };
    let (kind, subtype, hd) = node(0)?;
    if (kind, subtype) != (MEDIA_DEVICE_PATH, MEDIA_HARDDRIVE_DP) || hd.len() != HD_NODE_LEN - 4 {
        return Err("device path does not start with HD()".into());
    }
    if hd[36] != MBR_TYPE_GPT || hd[37] != SIGNATURE_TYPE_GUID {
        return Err("HD() is not a GPT partition".into());
    }
    let array = |at: usize| -> [u8; 8] {
        let mut out = [0; 8];
        out.copy_from_slice(&hd[at..at + 8]);
        out
    };
    let u64_at = |at: usize| u64::from_le_bytes(array(at));
    let mut signature = [0_u8; 16];
    signature.copy_from_slice(&hd[20..36]);
    let mut at = HD_NODE_LEN;
    let mut file_path = String::new();
    loop {
        let (kind, subtype, body) = node(at)?;
        at += 4 + body.len();
        match (kind, subtype) {
            (MEDIA_DEVICE_PATH, MEDIA_FILEPATH_DP) => {
                let (part, used) = read_utf16z(body)?;
                if used != body.len() {
                    return Err("File() node has trailing bytes".into());
                }
                if !file_path.is_empty() && !file_path.ends_with('\\') && !part.starts_with('\\') {
                    file_path.push('\\');
                }
                file_path.push_str(&part);
            }
            (END_DEVICE_PATH_TYPE, END_ENTIRE_DEVICE_PATH_SUBTYPE) if body.is_empty() => break,
            _ => return Err("unsupported device path node".into()),
        }
    }
    if at != list_len || file_path.is_empty() {
        return Err("device path list is not HD/File/End".into());
    }
    Ok(HardDriveLoadOption {
        description,
        partition_number: u32::from_le_bytes([hd[0], hd[1], hd[2], hd[3]]),
        start_lba: u64_at(4),
        size_lba: u64_at(12),
        partition_guid: guid_text(&signature),
        file_path,
    })
}

/// Attributes word of a load option.
pub fn load_option_attributes(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// Lowercase hex, as stored in records.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTF-16LE of ASCII text, NUL-terminated — written out by hand below
    /// where the fixture must not depend on the encoder.
    fn fixture_option() -> HardDriveLoadOption {
        HardDriveLoadOption {
            description: "Tilo installer".into(),
            partition_number: 5,
            start_lba: 0x0d40_0800,
            size_lba: 0x0012_c000,
            partition_guid: "3f0e2d1c-4b5a-4796-a8b9-c0d1e2f30415".into(),
            file_path: "\\EFI\\BOOT\\BOOTX64.EFI".into(),
        }
    }

    /// Hand-assembled from the UEFI 2.10 structure definitions (§3.1.3
    /// `EFI_LOAD_OPTION`, §10.3.5.1 Hard Drive, §10.3.5.4 File Path,
    /// §10.3.1 End). Field order and widths match efivar's
    /// `efidp_make_hd`/`efidp_make_file`, which `efibootmgr -c -d DISK -p 5
    /// -L "Tilo installer" -l '\EFI\BOOT\BOOTX64.EFI'` uses; efibootmgr adds no
    /// optional data unless `-@`/`-u` is given. The GUID is stored
    /// mixed-endian as in `efi_guid_t`.
    const FIXTURE: &[u8] = &[
        // Attributes = LOAD_OPTION_ACTIVE
        0x01, 0x00, 0x00, 0x00, //
        // FilePathListLength = 42 (HD) + 48 (File: 4 + 22 units * 2) + 4 (End) = 94
        0x5e, 0x00, //
        // Description "Tilo installer\0"
        b'T', 0, b'i', 0, b'l', 0, b'o', 0, b' ', 0, b'i', 0, b'n', 0, b's', 0, b't', 0, b'a', 0,
        b'l', 0, b'l', 0, b'e', 0, b'r', 0, 0, 0, //
        // HD: type 4, subtype 1, length 42
        0x04, 0x01, 0x2a, 0x00, //
        // PartitionNumber 5
        0x05, 0x00, 0x00, 0x00, //
        // PartitionStart 0x0d400800
        0x00, 0x08, 0x40, 0x0d, 0x00, 0x00, 0x00, 0x00, //
        // PartitionSize 0x0012c000
        0x00, 0xc0, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, //
        // Signature 3f0e2d1c-4b5a-4796-a8b9-c0d1e2f30415 (mixed-endian)
        0x1c, 0x2d, 0x0e, 0x3f, 0x5a, 0x4b, 0x96, 0x47, 0xa8, 0xb9, 0xc0, 0xd1, 0xe2, 0xf3, 0x04,
        0x15, //
        // MBRType = GPT, SignatureType = GUID
        0x02, 0x02, //
        // File: type 4, subtype 4, length 48
        0x04, 0x04, 0x30, 0x00, //
        // "\EFI\BOOT\BOOTX64.EFI\0"
        b'\\', 0, b'E', 0, b'F', 0, b'I', 0, b'\\', 0, b'B', 0, b'O', 0, b'O', 0, b'T', 0, b'\\', 0,
        b'B', 0, b'O', 0, b'O', 0, b'T', 0, b'X', 0, b'6', 0, b'4', 0, b'.', 0, b'E', 0, b'F', 0,
        b'I', 0, 0, 0, //
        // End Entire Device Path
        0x7f, 0xff, 0x04, 0x00,
    ];

    /// Encoder and decoder of EFI load options.
    #[test]
    fn efi_load_option_codec_round_trips_and_rejects_unsafe_data() {
        // Phase `encoder_matches_the_hand_assembled_fixture_byte_for_byte`.
        {
            assert_eq!(FIXTURE.len(), 4 + 2 + 30 + 94);
            let bytes = encode_load_option(&fixture_option()).unwrap();
            assert_eq!(hex(&bytes), hex(FIXTURE));
        }

        // Phase `decoder_round_trips_the_fixture`.
        {
            assert_eq!(decode_load_option(FIXTURE).unwrap(), fixture_option());
            assert_eq!(load_option_attributes(FIXTURE), Some(LOAD_OPTION_ACTIVE));
        }

        // Phase `decoder_accepts_optional_data_and_split_file_nodes`.
        {
            // Firmware sometimes writes one File() node per path component.
            let mut option = Vec::new();
            option.extend(1_u32.to_le_bytes());
            let hd = &FIXTURE[36..78];
            let file = |text: &str| {
                let units: Vec<u8> = text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                let mut node = vec![4, 4];
                node.extend(((4 + units.len()) as u16).to_le_bytes());
                node.extend(units);
                node
            };
            let mut path = hd.to_vec();
            path.extend(file("\\EFI\\BOOT"));
            path.extend(file("BOOTX64.EFI"));
            path.extend([0x7f, 0xff, 4, 0]);
            option.extend((path.len() as u16).to_le_bytes());
            option.extend(&FIXTURE[6..36]);
            option.extend(path);
            option.extend(b"optional");
            assert_eq!(decode_load_option(&option).unwrap(), fixture_option());
        }

        // Phase `decoder_rejects_malformed_options`.
        {
            assert!(decode_load_option(&FIXTURE[..40]).is_err());
            let mut long = FIXTURE.to_vec();
            long[4] = 0xff;
            assert!(decode_load_option(&long).is_err());
            let mut pci = FIXTURE.to_vec();
            pci[36] = 0x01; // hardware device path, not HD()
            assert!(decode_load_option(&pci).is_err());
            let mut mbr = FIXTURE.to_vec();
            mbr[36 + 40] = 0x01;
            assert!(decode_load_option(&mbr).is_err());
            let mut unterminated = FIXTURE[..6].to_vec();
            unterminated.extend([b'T', 0]);
            assert!(decode_load_option(&unterminated).is_err());
        }

        // Phase `encoder_refuses_unsafe_inputs`.
        {
            let mut option = fixture_option();
            option.file_path = "EFI/BOOT/BOOTX64.EFI".into();
            assert!(encode_load_option(&option).is_err());
            let mut option = fixture_option();
            option.partition_guid = "not-a-guid".into();
            assert!(encode_load_option(&option).is_err());
            let mut option = fixture_option();
            option.description = "a\0b".into();
            assert!(encode_load_option(&option).is_err());
            let mut option = fixture_option();
            option.partition_number = 0;
            assert!(encode_load_option(&option).is_err());
        }
    }

    /// GUID layout and boot number allocation.
    #[test]
    fn efi_guids_and_boot_numbers_follow_firmware_layout() {
        // Phase `guid_bytes_follow_efi_guid_layout`.
        {
            let g = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
            let bytes = guid_bytes(g).unwrap();
            assert_eq!(
                hex(&bytes),
                "61dfe48bca93d211aa0d00e098032b8c",
                "EFI_GLOBAL_VARIABLE as stored in memory"
            );
            assert_eq!(guid_text(&bytes), g);
            assert_eq!(guid_bytes(&g.to_ascii_uppercase()).unwrap(), bytes);
        }

        // Phase `boot_numbers_skip_order_and_existing_entries`.
        {
            let order = [0x0001, 0x0000, 0x0100];
            let existing = [0x0101_u16, 0x0103];
            let n = allocate_boot_number(&order, |n| Ok(existing.contains(&n))).unwrap();
            assert_eq!(n, 0x0102);
            assert_eq!(allocate_boot_number(&[], |_| Ok(false)).unwrap(), 0x0100);
            assert!(allocate_boot_number(&[], |_| Err("io".into())).is_err());
            assert_eq!(boot_var_name(0x0102), "Boot0102");
            assert_eq!(format_boot_number(0xab), "00AB");
            assert_eq!(parse_boot_number("00ab").unwrap(), 0xab);
            assert!(parse_boot_number("12").is_err());
            assert_eq!(parse_u16_list(&[1, 0, 0, 1]).unwrap(), vec![1, 0x100]);
            assert!(parse_u16_list(&[1]).is_err());
        }
    }
}

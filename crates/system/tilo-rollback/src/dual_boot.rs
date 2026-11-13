//! Does this machine also boot Windows?
//!
//! The TPM's dictionary-attack state (reset authorisation, `maxTries`/`recoveryTime`/
//! `lockoutRecovery`) is global to the chip. Windows provisions it on start whenever the reset
//! authorisation is empty; if Tilo got there first, Windows can no longer provision its TPM or
//! reset a lockout, and its only remedy, "Clear TPM", also destroys Tilo's keyslots and rollback
//! floor. So on a machine that boots Windows, [`crate::lockout`]'s global settings belong to
//! Windows.
//!
//! Discovery is by content: the load options are parsed as the UEFI `EFI_LOAD_OPTION` structure
//! (spec 3.1.3, device path nodes 10.3), so a description or optional data that merely mentions
//! `bootmgfw.efi` does not count.

use std::path::Path;

pub const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// `EFI_GLOBAL_VARIABLE`, the vendor GUID of every `Boot####` variable.
const GLOBAL_VARIABLE_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// The file path Windows' boot manager is always registered under.
const WINDOWS_BOOT_MANAGER: &str = "\\efi\\microsoft\\boot\\bootmgfw.efi";

/// Is any firmware boot entry Windows' boot manager?
///
/// A machine without efivarfs (BIOS boot, a container) reports `false`: there is no firmware
/// boot entry to look at.
pub fn windows_is_installed(efivars: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(efivars) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        is_boot_option_name(&entry.file_name().to_string_lossy())
            && std::fs::read(entry.path()).is_ok_and(|bytes| {
                // efivarfs prefixes the value with its four attribute bytes.
                bytes
                    .get(4..)
                    .and_then(load_option_file_path)
                    .is_some_and(|path| is_windows_boot_manager(&path))
            })
    })
}

fn is_boot_option_name(name: &str) -> bool {
    name.strip_suffix(GLOBAL_VARIABLE_GUID)
        .and_then(|n| n.strip_suffix('-'))
        .and_then(|n| n.strip_prefix("Boot"))
        .is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_windows_boot_manager(path: &str) -> bool {
    path.replace('/', "\\").to_ascii_lowercase() == WINDOWS_BOOT_MANAGER
}

/// The file path an `EFI_LOAD_OPTION` loads, or `None` when it has none (a whole-disk or network
/// entry, a firmware application) or is malformed.
///
/// Layout: `u32` attributes, `u16` length of the device path list, a NUL-terminated UTF-16
/// description, then the device path list. Only the first instance is booted; its media file
/// path nodes (type 4, subtype 4) are concatenated, since a path may be split across nodes.
pub fn load_option_file_path(option: &[u8]) -> Option<String> {
    let (_attributes, rest) = option.split_at_checked(4)?;
    let (list_len, mut rest) = rest.split_first_chunk::<2>()?;
    let list_len = usize::from(u16::from_le_bytes(*list_len));
    loop {
        let (unit, after) = rest.split_first_chunk::<2>()?;
        rest = after;
        if *unit == [0, 0] {
            break;
        }
    }
    let (mut list, _optional_data) = rest.split_at_checked(list_len)?;
    let mut path = String::new();
    while let Some((&[kind, subtype, l0, l1], _)) = list.split_first_chunk::<4>() {
        let (node, after) = list.split_at_checked(usize::from(u16::from_le_bytes([l0, l1])))?;
        let body = node.get(4..)?;
        if kind == 0x7f {
            // End of this instance (0x01) or of the whole path (0xff): the first is complete.
            break;
        }
        if kind == 0x04 && subtype == 0x04 {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .filter_map(<[u8]>::first_chunk::<2>)
                .map(|c| u16::from_le_bytes(*c))
                .take_while(|&u| u != 0)
                .collect();
            path.push_str(&String::from_utf16(&units).ok()?);
        }
        list = after;
    }
    (!path.is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    fn node(kind: u8, subtype: u8, body: &[u8]) -> Vec<u8> {
        let len = u16::try_from(body.len().checked_add(4).unwrap()).unwrap();
        let mut n = vec![kind, subtype];
        n.extend_from_slice(&len.to_le_bytes());
        n.extend_from_slice(body);
        n
    }

    /// A GPT hard-drive media node.
    fn hd() -> Vec<u8> {
        node(0x04, 0x01, &[0u8; 38])
    }

    fn end() -> Vec<u8> {
        node(0x7f, 0xff, &[])
    }

    fn load_option(description: &str, nodes: &[Vec<u8>], optional: &[u8]) -> Vec<u8> {
        let list: Vec<u8> = nodes.concat();
        let mut o = 1u32.to_le_bytes().to_vec();
        o.extend_from_slice(&u16::try_from(list.len()).unwrap().to_le_bytes());
        o.extend(utf16z(description));
        o.extend(list);
        o.extend_from_slice(optional);
        o
    }

    fn file(path: &str) -> Vec<u8> {
        node(0x04, 0x04, &utf16z(path))
    }

    #[test]
    fn firmware_boot_entries_identify_windows_by_its_loader_path() {
        let windows = load_option(
            "Windows Boot Manager",
            &[hd(), file("\\EFI\\Microsoft\\Boot\\bootmgfw.efi"), end()],
            b"WINDOWS\0",
        );
        let path = load_option_file_path(&windows).unwrap();
        assert_eq!(path, "\\EFI\\Microsoft\\Boot\\bootmgfw.efi");
        assert!(is_windows_boot_manager(&path));

        let tilo = load_option(
            "Tilo",
            &[hd(), file("\\EFI\\systemd\\systemd-bootaa64.efi"), end()],
            &[],
        );
        assert!(!is_windows_boot_manager(
            &load_option_file_path(&tilo).unwrap()
        ));

        let disk = load_option("EFI Hard Drive", &[hd(), end()], b"RC");
        assert_eq!(load_option_file_path(&disk), None);

        let mention = load_option(
            "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
            &[hd(), end()],
            &utf16z("\\EFI\\Microsoft\\Boot\\bootmgfw.efi"),
        );
        assert_eq!(load_option_file_path(&mention), None);

        let split = load_option(
            "split",
            &[
                hd(),
                file("\\EFI\\Microsoft"),
                file("\\Boot\\bootmgfw.efi"),
                node(0x7f, 0x01, &[]),
                file("\\EFI\\other.efi"),
                end(),
            ],
            &[],
        );
        assert!(is_windows_boot_manager(
            &load_option_file_path(&split).unwrap()
        ));

        assert_eq!(load_option_file_path(windows.get(..40).unwrap()), None);
        let mut bad = windows;
        let list_start = 6 + utf16z("Windows Boot Manager").len();
        bad[list_start + 2] = 2; // a node shorter than its own header
        bad[list_start + 3] = 0;
        assert_eq!(load_option_file_path(&bad), None);
    }

    #[test]
    fn only_boot_option_variables_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let windows = load_option(
            "Windows Boot Manager",
            &[hd(), file("\\EFI\\Microsoft\\Boot\\bootmgfw.efi"), end()],
            &[],
        );
        let with_attributes = [7u32.to_le_bytes().to_vec(), windows].concat();

        std::fs::write(
            dir.path().join(format!("BootOrder-{GLOBAL_VARIABLE_GUID}")),
            &with_attributes,
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("Boot0003-00000000-0000-0000-0000-000000000000"),
            &with_attributes,
        )
        .unwrap();
        assert!(!windows_is_installed(dir.path()));

        std::fs::write(
            dir.path().join(format!("Boot0003-{GLOBAL_VARIABLE_GUID}")),
            &with_attributes,
        )
        .unwrap();
        assert!(windows_is_installed(dir.path()));

        assert!(!windows_is_installed(&dir.path().join("absent")));
    }
}

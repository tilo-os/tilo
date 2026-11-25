//! The partitions a Tilo install lays out, read when this crate is built from
//! `os/usr/share/tilo/install-sizes`: the file the installer's scripts source. The sizes the
//! imager offers and plans with and the sizes the scripts check before writing a disk are one.

const INSTALL_SIZES: &str = include_str!("../../../os/usr/share/tilo/install-sizes");

/// The EFI system partition: several signed UKIs for rollback and repair.
pub const ESP_BYTES: u64 = size("TILO_ESP_BYTES");
/// The boot partition: the kernel and initramfs of several boot entries.
pub const BOOT_BYTES: u64 = size("TILO_BOOT_BYTES");
/// The smallest encrypted root, on an erased disk or beside another system.
pub const MIN_ROOT_BYTES: u64 = size("TILO_MIN_ROOT_BYTES");
/// The least room Tilo takes on a disk.
pub const MIN_TILO_BYTES: u64 = ESP_BYTES + BOOT_BYTES + MIN_ROOT_BYTES;

/// The size the file sets as `name=<bytes>`. A size it lacks or writes in anything but whole
/// bytes stops the build.
#[expect(
    clippy::panic,
    reason = "a size the file lacks stops the build, not an install"
)]
const fn size(name: &str) -> u64 {
    let mut rest = INSTALL_SIZES.as_bytes();
    while !rest.is_empty() {
        if let Some(value) = setting(rest, name.as_bytes()) {
            return bytes(value);
        }
        rest = next_line(rest);
    }
    panic!("os/usr/share/tilo/install-sizes lacks a size the installer reads")
}

/// What `line` sets `name` to, when it does.
const fn setting<'a>(mut line: &'a [u8], mut name: &[u8]) -> Option<&'a [u8]> {
    loop {
        match (line, name) {
            ([b'=', value @ ..], []) => return Some(value),
            ([have, line_rest @ ..], [want, name_rest @ ..]) if *have == *want => {
                line = line_rest;
                name = name_rest;
            }
            _ => return None,
        }
    }
}

#[expect(
    clippy::panic,
    reason = "a size the file writes wrongly stops the build, not an install"
)]
const fn bytes(mut text: &[u8]) -> u64 {
    let mut value: u64 = 0;
    let mut any = false;
    while let [digit @ b'0'..=b'9', rest @ ..] = text {
        value = match value.checked_mul(10) {
            Some(tens) => match tens.checked_add(digit.abs_diff(b'0') as u64) {
                Some(sum) => sum,
                None => panic!("an install size does not fit 64 bits"),
            },
            None => panic!("an install size does not fit 64 bits"),
        };
        any = true;
        text = rest;
    }
    assert!(
        any && matches!(text, [] | [b'\n', ..]),
        "an install size is not a whole number of bytes"
    );
    value
}

const fn next_line(mut text: &[u8]) -> &[u8] {
    while let [byte, rest @ ..] = text {
        text = rest;
        if *byte == b'\n' {
            break;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_is_read_by_its_whole_name() {
        let text = b"TILO_ESP_BYTES_FREE=1\nTILO_ESP_BYTES=2147483648\n";
        assert_eq!(setting(text, b"TILO_ESP_BYTES"), None);
        let second = next_line(text);
        assert_eq!(
            setting(second, b"TILO_ESP_BYTES").map(bytes),
            Some(2_147_483_648)
        );
        assert_eq!(setting(b"# TILO_ESP_BYTES=1", b"TILO_ESP_BYTES"), None);
    }

    #[test]
    fn the_least_room_is_the_partitions_summed() {
        assert_eq!(MIN_TILO_BYTES, ESP_BYTES + BOOT_BYTES + MIN_ROOT_BYTES);
        assert_eq!(MIN_TILO_BYTES, 35 << 30);
    }
}

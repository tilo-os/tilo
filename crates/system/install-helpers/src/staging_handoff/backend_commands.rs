//! The installer backend's own small checks and writes: the shell scripts
//! (`tilo-install`, `tilo-guided-partition`, `tilo-installer-profile-extract`)
//! hand each one here instead of to an interpreter.
//!
//! * `check-device-label LABEL`: at most 64 UTF-8 bytes, no control
//!   characters.
//! * `write-machine-info PATH LABEL`: replace `PRETTY_HOSTNAME=` in
//!   machine-info, keeping every other line, atomically and durably.
//! * `check-prepared-choices --username … --keyvariant …`: the authenticated
//!   prepared setup on stdin must carry the choices the backend was given.
//! * `guided-geometry DISK SECTOR FREE_START FREE_SIZE EFI_START EFI_SIZE
//!   BOOT_START BOOT_SIZE ROOT_START ROOT_SIZE [UUID:START:SIZE…]`: the
//!   reviewed guided allocation, checked with unbounded integers before the
//!   shell's signed arithmetic sees it.
//! * `gpt map|tilo-roots|number-node UUID|node UUID|type UUID`: facts from
//!   `sfdisk --json` on stdin.
//! * `unpack-setup-handoff PAYLOAD DEST`: the imager's setup handoff payload
//!   (`PATH\nSIZE\nBYTES` records) unpacked below DEST.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::retained_commands::parse_receipt;
use super::{Fail, Outcome, Result, refuse};

/// The commands this module answers.
pub const COMMANDS: [&str; 6] = [
    "check-device-label",
    "write-machine-info",
    "check-prepared-choices",
    "guided-geometry",
    "gpt",
    "unpack-setup-handoff",
];

const TILO_ROOT_TYPE: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";
const SFDISK_MAX_BYTES: u64 = 1024 * 1024;
const MIB: u128 = 1 << 20;
const GIB: u128 = 1 << 30;
const EFI_BYTES: u128 = 2 << 30;
const MIN_ROOT_BYTES: u128 = 32 << 30;
/// Every byte count must fit a signed 64-bit integer: the shell and the
/// kernel use that range.
const MAX_BYTES: u128 = (1 << 63) - 1;

fn text(arg: &OsStr) -> Result<&str> {
    arg.to_str()
        .ok_or_else(|| Fail::Refusal("argument is not UTF-8".into()))
}

/// `check-device-label`: what machine-info and the enrolment certificate can
/// carry as a display name.
///
/// # Errors
/// A label longer than 64 bytes or with a C0/C1 control character.
pub fn check_device_label(label: &OsStr) -> Result<()> {
    let label = text(label)?;
    if label.len() > 64
        || label
            .chars()
            .any(|c| c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))
    {
        return refuse("Invalid device label");
    }
    Ok(())
}

/// Python's `str.splitlines()`: every Unicode line boundary, `\r\n` as one.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(text.get(start..index).unwrap_or_default());
        let mut end = index.saturating_add(c.len_utf8());
        if c == '\r'
            && let Some((next, '\n')) = chars.peek().copied()
        {
            chars.next();
            end = next.saturating_add(1);
        }
        start = end;
    }
    if start < text.len() {
        lines.push(text.get(start..).unwrap_or_default());
    }
    lines
}

/// Python's `str.isspace()` for one character.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The machine-info text with `label` as the only `PRETTY_HOSTNAME=`, quoted
/// for systemd's shell-like parser.
fn machine_info(existing: &str, label: &str) -> String {
    let mut out = String::new();
    for line in split_lines(existing) {
        if !line
            .trim_start_matches(python_space)
            .starts_with("PRETTY_HOSTNAME=")
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("PRETTY_HOSTNAME=\"");
    for c in label.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str("\"\n");
    out
}

/// `write-machine-info`: publish the new machine-info under a temporary name
/// in the same directory, synced, renamed over the old one, and the
/// directory synced.
///
/// # Errors
/// A symlinked machine-info, an unreadable one, or a failed write.
pub fn write_machine_info(path: &Path, label: &OsStr) -> Result<()> {
    let label = text(label)?;
    if path.is_symlink() {
        return refuse("machine-info is a symbolic link");
    }
    let existing = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(machine_info(&existing, label).as_bytes())?;
    temporary.flush()?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Fail::Os(error.error.to_string()))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// The choices `tilo-install` was given, by flag.
const CHOICE_FLAGS: [&str; 10] = [
    "--username",
    "--full-name",
    "--device-label",
    "--purpose",
    "--hostname",
    "--locale",
    "--timezone",
    "--filesystem",
    "--keymap",
    "--keyvariant",
];

fn choices(args: &[OsString]) -> Result<[String; 10]> {
    let mut values: [Option<String>; 10] = Default::default();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let slot = CHOICE_FLAGS
            .iter()
            .position(|known| OsStr::new(known) == flag)
            .and_then(|index| values.get_mut(index))
            .ok_or_else(|| Fail::Refusal(format!("unknown argument {}", flag.display())))?;
        let value = args
            .next()
            .ok_or_else(|| Fail::Refusal(format!("{} needs a value", flag.display())))?;
        if slot.replace(text(value)?.to_owned()).is_some() {
            return refuse(format!("{} given twice", flag.display()));
        }
    }
    let mut out: [String; 10] = Default::default();
    for ((value, flag), slot) in values.into_iter().zip(CHOICE_FLAGS).zip(out.iter_mut()) {
        *slot = value.ok_or_else(|| Fail::Refusal(format!("missing {flag}")))?;
    }
    Ok(out)
}

/// `check-prepared-choices`: an install profile in the authenticated
/// prepared setup must equal the choices the backend acts on. The keyboard
/// is compared as the backend splits it: Dvorak and Colemak are US variants.
///
/// # Errors
/// Malformed choices or a profile that differs from them.
pub fn check_prepared_choices(args: &[OsString], input: impl std::io::Read) -> Result<()> {
    let [
        username,
        full_name,
        device_label,
        purpose,
        hostname,
        locale,
        timezone,
        filesystem,
        keymap,
        keyvariant,
    ] = choices(args)?;
    let receipt = parse_receipt(input)?;
    let profile = match receipt.get("install_profile") {
        None => return refuse("authenticated prepared choices have no install profile"),
        Some(Value::Null) => return Ok(()),
        Some(Value::Object(profile)) => profile,
        Some(_) => return refuse("authenticated install profile is malformed"),
    };
    let is = |key: &str, expected: &str| profile.get(key).and_then(Value::as_str) == Some(expected);
    let or_default = |key: &str, default: &str, expected: &str| match profile.get(key) {
        None => default == expected,
        Some(value) => value.as_str() == Some(expected),
    };
    if !(is("username", &username) && is("full_name", &full_name)) {
        return refuse("prepared account differs from authenticated choices");
    }
    if !or_default("device_label", "", &device_label) {
        return refuse("prepared device label differs from authenticated choices");
    }
    if !or_default("purpose", "computer", &purpose) {
        return refuse("prepared purpose differs from authenticated choices");
    }
    if !(is("hostname", &hostname)
        && is("locale", &locale)
        && is("timezone", &timezone)
        && is("filesystem", &filesystem))
    {
        return refuse("prepared preferences differ from authenticated choices");
    }
    let keyboard = profile.get("keyboard").and_then(Value::as_str);
    let expected = match keyboard {
        Some(variant @ ("dvorak" | "colemak")) => Some(("us", variant)),
        Some(layout) => Some((layout, "")),
        None => None,
    };
    if expected != Some((keymap.as_str(), keyvariant.as_str())) {
        return refuse("prepared keyboard differs from authenticated choices");
    }
    Ok(())
}

/// A positive decimal byte count in the signed 64-bit range.
fn geometry_number(raw: &str) -> std::result::Result<u128, &'static str> {
    let canonical = raw
        .bytes()
        .next()
        .is_some_and(|b| (b'1'..=b'9').contains(&b))
        && raw.bytes().all(|b| b.is_ascii_digit());
    if !canonical {
        return Err("geometry must use positive decimal bytes");
    }
    match raw.parse::<u128>() {
        Ok(value) if value <= MAX_BYTES => Ok(value),
        _ => Err("geometry exceeds the supported byte range"),
    }
}

/// `[start, end)` of a byte extent.
fn geometry_extent(start: &str, size: &str) -> std::result::Result<(u128, u128), &'static str> {
    let (start, size) = (geometry_number(start)?, geometry_number(size)?);
    let end = start.saturating_add(size);
    if end > MAX_BYTES {
        return Err("extent end overflows the supported byte range");
    }
    Ok((start, end))
}

/// The guided allocation rules: inside the disk's usable area, MiB aligned,
/// a 2 GiB ESP first, an optional boot of at least 1 GiB, then a root of at
/// least 32 GiB filling the rest, clear of every existing partition.
///
/// # Errors
/// The first rule the allocation breaks.
pub fn guided_geometry(args: &[&str]) -> std::result::Result<(), String> {
    let [
        disk,
        sector,
        free_start,
        free_size,
        efi_start,
        efi_size,
        boot_start,
        boot_size,
        root_start,
        root_size,
        existing @ ..,
    ] = args
    else {
        return Err("the allocation is incomplete".into());
    };
    let disk = geometry_number(disk)?;
    let sector = geometry_number(sector)?;
    if sector != 512 && sector != 4096 {
        return Err("unsupported logical sector size".into());
    }
    let free = geometry_extent(free_start, free_size)?;
    let efi = geometry_extent(efi_start, efi_size)?;
    let boot = if boot_start.is_empty() && boot_size.is_empty() {
        None
    } else {
        Some(geometry_extent(boot_start, boot_size)?)
    };
    let root = geometry_extent(root_start, root_size)?;
    for (start, end) in [Some(free), Some(efi), Some(root), boot]
        .into_iter()
        .flatten()
    {
        if start % MIB != 0 || end % MIB != 0 {
            return Err("allocation must be MiB aligned".into());
        }
    }
    if free.0 < MIB || free.1 > disk.saturating_sub(MIB) {
        return Err("allocation overlaps GPT metadata or exceeds the disk".into());
    }
    if efi.0 != free.0 || efi.1.saturating_sub(efi.0) != EFI_BYTES {
        return Err("allocation requires a dedicated 2 GiB EFI partition".into());
    }
    let mut previous = efi.1;
    if let Some(boot) = boot {
        if boot.0 != previous || boot.1.saturating_sub(boot.0) < GIB {
            return Err("allocation requires a contiguous 1 GiB boot partition".into());
        }
        previous = boot.1;
    }
    if root.0 != previous || root.1 != free.1 || root.1.saturating_sub(root.0) < MIN_ROOT_BYTES {
        return Err("root must fill the reviewed allocation and hold at least 32 GiB".into());
    }
    for item in existing {
        let parts: Vec<&str> = item.split(':').collect();
        let [_, start, size] = parts.as_slice() else {
            return Err("allocation overlaps an existing partition or its map is invalid".into());
        };
        let occupied = geometry_extent(start, size)?;
        if occupied.1 > disk || !(occupied.1 <= free.0 || occupied.0 >= free.1) {
            return Err("allocation overlaps an existing partition or its map is invalid".into());
        }
    }
    Ok(())
}

/// `int(value)` for an sfdisk count: a JSON integer or a decimal string.
fn sfdisk_int(value: Option<&Value>) -> Option<i128> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn sfdisk_text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase()
}

/// `gpt`: one fact from `sfdisk --json` on stdin. `Ok(None)` is a quiet
/// "not found" (exit 1).
///
/// # Errors
/// Unparseable sfdisk output or an incomplete partition identity.
pub fn gpt(query: &str, uuid: Option<&str>, input: impl std::io::Read) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    input.take(SFDISK_MAX_BYTES).read_to_end(&mut bytes)?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Fail::Refusal("sfdisk output is malformed".into()))?;
    let table = document
        .get("partitiontable")
        .ok_or_else(|| Fail::Refusal("sfdisk output has no partition table".into()))?;
    let empty = Vec::new();
    let partitions = match table.get("partitions") {
        None => &empty,
        Some(Value::Array(partitions)) => partitions,
        Some(_) => return refuse("sfdisk partitions are malformed"),
    };
    let wanted = uuid.map(str::to_lowercase).unwrap_or_default();
    let matching = || {
        partitions
            .iter()
            .filter(|part| sfdisk_text(part.get("uuid")) == wanted)
    };
    match (query, uuid) {
        ("map", None) => {
            let sector = sfdisk_int(table.get("sectorsize"))
                .ok_or_else(|| Fail::Refusal("sfdisk reports no sector size".into()))?;
            let mut out = String::new();
            for part in partitions {
                let uuid = sfdisk_text(part.get("uuid"));
                let count = |key: &str| match part.get(key) {
                    None => Some(0),
                    value => sfdisk_int(value).and_then(|count| count.checked_mul(sector)),
                };
                let (start, size) = (count("start"), count("size"));
                let (Some(start), Some(size)) = (start, size) else {
                    return refuse("incomplete GPT partition identity");
                };
                if uuid.is_empty() || start <= 0 || size <= 0 {
                    return refuse("incomplete GPT partition identity");
                }
                let _ = writeln!(out, "{uuid}:{start}:{size}");
            }
            Ok(Some(out))
        }
        ("tilo-roots", None) => {
            let roots = partitions
                .iter()
                .filter(|part| sfdisk_text(part.get("type")) == TILO_ROOT_TYPE)
                .count();
            Ok(Some(format!("{roots}\n")))
        }
        ("number-node", Some(_)) => {
            let found: Vec<&Value> = matching().collect();
            let [part] = found.as_slice() else {
                return Ok(None);
            };
            let node = part.get("node").and_then(Value::as_str).unwrap_or_default();
            let digits = node
                .len()
                .saturating_sub(node.bytes().rev().take_while(u8::is_ascii_digit).count());
            let number = node.get(digits..).unwrap_or_default();
            if number.is_empty() {
                return Ok(None);
            }
            Ok(Some(format!("{number} {node}\n")))
        }
        ("node", Some(_)) => Ok(matching()
            .next()
            .and_then(|part| part.get("node"))
            .and_then(Value::as_str)
            .map(|node| format!("{node}\n"))),
        ("type", Some(_)) => Ok(matching()
            .next()
            .map(|part| format!("{}\n", sfdisk_text(part.get("type"))))),
        _ => refuse("usage: gpt map|tilo-roots|number-node UUID|node UUID|type UUID"),
    }
}

/// The path of one handoff record below the destination: relative, without
/// `..`, and naming a file rather than the destination itself.
fn handoff_relative(raw: &[u8]) -> Option<PathBuf> {
    let relative = std::str::from_utf8(raw).ok()?;
    if relative.starts_with('/') {
        return None;
    }
    let mut path = PathBuf::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The bytes before the first newline, and those after it.
fn split_line(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = data.iter().position(|&b| b == b'\n')?;
    Some((data.get(..end)?, data.get(end.saturating_add(1)..)?))
}

/// `unpack-setup-handoff`: every `PATH\nSIZE\nBYTES` record of the payload
/// written below `destination`.
///
/// # Errors
/// A truncated or malformed record, an absolute or escaping path, or a
/// failed write.
pub fn unpack_setup_handoff(payload: &[u8], destination: &Path) -> Result<()> {
    let malformed = || Fail::Refusal("setup handoff payload is malformed".into());
    let mut rest = payload;
    while !rest.is_empty() {
        let (relative, after) = split_line(rest).ok_or_else(malformed)?;
        let (size, after) = split_line(after).ok_or_else(malformed)?;
        let size: usize = std::str::from_utf8(size)
            .ok()
            .filter(|size| !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|size| size.parse().ok())
            .ok_or_else(malformed)?;
        let chunk = after.get(..size).ok_or_else(malformed)?;
        let relative = handoff_relative(relative).ok_or_else(malformed)?;
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, chunk)?;
        rest = after.get(size..).unwrap_or_default();
    }
    Ok(())
}

fn ok(stdout: String) -> Outcome {
    Outcome {
        code: 0,
        stdout,
        stderr: String::new(),
    }
}

fn failed(stderr: impl std::fmt::Display) -> Outcome {
    Outcome {
        code: 1,
        stdout: String::new(),
        stderr: format!("{stderr}\n"),
    }
}

fn usage(message: &str) -> Outcome {
    Outcome {
        code: 2,
        stdout: String::new(),
        stderr: format!("usage: tilo-staging-handoff {message}\n"),
    }
}

fn done(result: Result<()>) -> Outcome {
    result.map_or_else(failed, |()| ok(String::new()))
}

#[must_use]
pub fn run(command: &str, args: &[OsString]) -> Outcome {
    let words: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().unwrap_or("\u{fffd}"))
        .collect();
    match (command, args) {
        ("check-device-label", [label]) => done(check_device_label(label)),
        ("check-device-label", _) => usage("check-device-label LABEL"),
        ("write-machine-info", [path, label]) => done(write_machine_info(Path::new(path), label)),
        ("write-machine-info", _) => usage("write-machine-info PATH LABEL"),
        ("check-prepared-choices", _) => {
            done(check_prepared_choices(args, std::io::stdin().lock()))
        }
        ("guided-geometry", _) => match guided_geometry(&words) {
            Ok(()) => ok(String::new()),
            Err(error) => failed(format!("guided allocation: {error}")),
        },
        ("gpt", [query, rest @ ..]) if rest.len() <= 1 => {
            let uuid = rest.first().map(|uuid| text(uuid));
            let uuid = match uuid.transpose() {
                Ok(uuid) => uuid,
                Err(error) => return failed(error),
            };
            match gpt(&query.to_string_lossy(), uuid, std::io::stdin().lock()) {
                Ok(Some(stdout)) => ok(stdout),
                Ok(None) => Outcome {
                    code: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                Err(error) => failed(error),
            }
        }
        ("gpt", _) => usage("gpt map|tilo-roots|number-node UUID|node UUID|type UUID"),
        ("unpack-setup-handoff", [payload, destination]) => done(
            fs::read(payload)
                .map_err(Fail::from)
                .and_then(|payload| unpack_setup_handoff(&payload, Path::new(destination))),
        ),
        _ => usage("unpack-setup-handoff PAYLOAD DEST"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_as_python_splits_them() {
        assert_eq!(
            split_lines("a\r\nb\rc\u{2028}d\n\ne"),
            ["a", "b", "c", "d", "", "e"]
        );
        assert_eq!(split_lines("a\n"), ["a"]);
        assert!(split_lines("").is_empty());
    }

    #[test]
    fn machine_info_replaces_only_the_pretty_hostname() {
        assert_eq!(
            machine_info(
                "CHASSIS=laptop\n  PRETTY_HOSTNAME=old\r\nICON_NAME=computer",
                "Ada's \"$HOME\" `x` \\"
            ),
            "CHASSIS=laptop\nICON_NAME=computer\n\
             PRETTY_HOSTNAME=\"Ada's \\\"\\$HOME\\\" \\`x\\` \\\\\"\n"
        );
        assert_eq!(machine_info("", "Desk"), "PRETTY_HOSTNAME=\"Desk\"\n");
    }

    #[test]
    fn device_labels_are_bounded_and_printable() {
        for good in ["", "Ada's laptop", "Café ☕", &"x".repeat(64)] {
            assert!(check_device_label(OsStr::new(good)).is_ok(), "{good}");
        }
        for bad in [
            "a\nb",
            "tab\t",
            "del\u{7f}",
            "c1\u{85}",
            &"x".repeat(65),
            &"é".repeat(33),
        ] {
            assert!(check_device_label(OsStr::new(bad)).is_err(), "{bad:?}");
        }
    }
}

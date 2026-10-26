//! `tilo-boot-order`: enlists a successfully booted Tilo after Windows in the
//! firmware boot order without changing the default.
//!
//! ```text
//! tilo-boot-order enlist --esp-partuuid GUID --boot-id XXXX --loader PATH
//! ```
//!
//! Run by hand, as root, after checking the first Tilo boot. The supplied
//! identities come from the physical install readback and its armed-boot
//! receipt, not discovery by label. This command never creates or replaces an
//! EFI boot option: it appends the named, currently booted Tilo option to the
//! end of `BootOrder` once every firmware fact matches, and reads the firmware
//! back afterwards. Exit status 0 when done (or already done), 1 on a refusal,
//! 2 on a usage error.
//!
//! Everything it asks of the machine (the EFI variables, `efibootmgr`, the
//! effective user) goes through [`Host`], whose provided methods are the
//! production behaviour; the tests stand in for the firmware with files.

mod args;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use rustix::fs::{CWD, FileType, Mode, OFlags, fstat, openat};
use rustix::io::Errno;

pub use args::{Parsed, Request, parse};

pub const EFI_GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
pub const LOADER_VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
pub const LOADERS: [&str; 2] = [
    r"\EFI\systemd\systemd-bootaa64.efi",
    r"\EFI\BOOT\BOOTAA64.EFI",
];
pub const WINDOWS_LOADER: &str = r"\EFI\Microsoft\Boot\bootmgfw.efi";
const VARIABLE_LIMIT: usize = 65536;
/// How long `efibootmgr` may take before it is treated as wedged: a firmware
/// write is slow, never open-ended.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// The most `efibootmgr` may write on either stream: one boot entry is a few
/// hundred bytes, and the list it prints is short.
const TOOL_OUTPUT_LIMIT: usize = 1024 * 1024;

/// Why the helper refused, or what the operating system refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

fn refuse<T>(message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal(message.into()))
}

/// `str(OSError)` as Python wrote it: `[Errno N] reason: 'path'`.
fn os_error(errno: Errno, path: &str) -> Refusal {
    let error = std::io::Error::from(errno);
    let text = error.to_string();
    let reason = text.split(" (os error").next().unwrap_or(&text).to_owned();
    Refusal(format!(
        "[Errno {}] {reason}: '{path}'",
        errno.raw_os_error()
    ))
}

/// The machine: EFI variables, `efibootmgr` and the effective user.
pub trait Host {
    /// Where the EFI variables are (efivarfs).
    fn efivars(&self) -> PathBuf {
        PathBuf::from("/sys/firmware/efi/efivars")
    }

    /// Runs `efibootmgr` with `args` and returns what it did: a firmware
    /// write is slow but finite, so the command has a deadline, bounded
    /// output and a process tree of its own, while its standard input stays
    /// the terminal it may ask for the firmware password on.
    ///
    /// # Errors
    /// `efibootmgr` could not be started, or did not finish within its
    /// deadline.
    fn efibootmgr(&self, args: &[&str]) -> Result<Output, Refusal> {
        let mut command = Command::new("efibootmgr");
        command.args(args).stdin(Stdio::inherit());
        let output = bounded_command::run_prepared_inheriting_stdin(
            command,
            TOOL_OUTPUT_LIMIT,
            TOOL_TIMEOUT,
        )
        .map_err(|error| match error {
            bounded_command::Error::Process(error) => Refusal(format!(
                "[Errno {}] {}: 'efibootmgr'",
                error.raw_os_error().unwrap_or(0),
                error
                    .to_string()
                    .split(" (os error")
                    .next()
                    .unwrap_or_default()
            )),
            // A wedged or voluminous firmware tool is not an operating-system
            // error: it is reported in its own words.
            other => Refusal(format!("efibootmgr could not be run: {other}")),
        })?;
        Ok(Output {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// The effective user id.
    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }
}

/// The machine itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

/// The payload of EFI variable `name` (after its four attribute bytes), or
/// `None` when it does not exist.
///
/// # Errors
/// A variable that is not a regular file, truncated or larger than 64 KiB,
/// or one that cannot be opened or read.
pub fn var(host: &dyn Host, name: &str, vendor: &str) -> Result<Option<Vec<u8>>, Refusal> {
    let path = host.efivars().join(format!("{name}-{vendor}"));
    let shown = path.display().to_string();
    let fd = match openat(
        CWD,
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(os_error(errno, &shown)),
    };
    let metadata = fstat(&fd).map_err(|errno| os_error(errno, &shown))?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return refuse(format!("{name} is not a regular EFI variable"));
    }
    // One read, as efivarfs hands over a whole variable at once.
    let mut data = vec![0u8; VARIABLE_LIMIT.saturating_add(1)];
    let read = loop {
        match rustix::io::read(fd.as_fd(), &mut data) {
            Ok(read) => break read,
            Err(Errno::INTR) => {}
            Err(errno) => return Err(os_error(errno, &shown)),
        }
    };
    data.truncate(read);
    if !(4..=VARIABLE_LIMIT).contains(&data.len()) {
        return refuse(format!("{name} is truncated or too large"));
    }
    Ok(Some(data.split_off(4)))
}

/// A list of `Boot####` numbers (`BootOrder`, `BootCurrent`, `BootNext`).
///
/// # Errors
/// See [`var`]; an odd length.
pub fn ids(host: &dyn Host, name: &str) -> Result<Option<Vec<String>>, Refusal> {
    let Some(raw) = var(host, name, EFI_GLOBAL)? else {
        return Ok(None);
    };
    if !raw.len().is_multiple_of(2) {
        return refuse(format!("{name} has odd length"));
    }
    Ok(Some(
        raw.chunks_exact(2)
            .map(|pair| match pair {
                [low, high] => format!("{:04X}", u16::from_le_bytes([*low, *high])),
                _ => String::new(),
            })
            .collect(),
    ))
}

/// One boot option: its description, its partition GUID and its loader path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOption {
    pub description: String,
    pub partuuid: String,
    pub loader: String,
}

fn utf16le(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.chunks_exact(2).map(|pair| match pair {
        [low, high] => u16::from_le_bytes([*low, *high]),
        _ => 0,
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*pair.first()?, *pair.get(1)?]))
}

/// `uuid.UUID(bytes_le=…)` as a lowercase hyphenated string.
fn guid_le(bytes: &[u8]) -> Option<String> {
    let b: &[u8; 16] = bytes.try_into().ok()?;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[3],
        b[2],
        b[1],
        b[0],
        b[5],
        b[4],
        b[7],
        b[6],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    ))
}

struct Node<'a> {
    kind: u8,
    subtype: u8,
    data: &'a [u8],
    size: usize,
}

/// `Boot####`: active, one GPT hard-drive node, one file node, the end node.
///
/// # Errors
/// An absent, inactive, truncated or malformed option, or one that does not
/// name exactly one GPT partition and EFI file.
pub fn option(host: &dyn Host, boot_id: &str) -> Result<BootOption, Refusal> {
    let name = format!("Boot{boot_id}");
    let raw = match var(host, &name, EFI_GLOBAL)? {
        Some(raw) if raw.len() >= 10 => raw,
        _ => return refuse(format!("{name} is absent or truncated")),
    };
    let attributes = raw
        .get(..4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes);
    let length = usize::from(u16_at(&raw, 4).unwrap_or(0));
    if attributes & 1 == 0 {
        return refuse(format!("{name} is inactive"));
    }
    let mut cursor = 6usize;
    while cursor.saturating_add(1) < raw.len()
        && raw.get(cursor..cursor.saturating_add(2)) != Some(&[0, 0][..])
    {
        cursor = cursor.saturating_add(2);
    }
    if cursor.saturating_add(2) > raw.len() {
        return refuse(format!("{name} has no description terminator"));
    }
    let description = utf16le(raw.get(6..cursor).unwrap_or_default())
        .ok_or_else(|| Refusal(format!("{name} description is invalid")))?;
    cursor = cursor.saturating_add(2);
    let path = raw.get(cursor..).unwrap_or_default();
    let path = path.get(..length.min(path.len())).unwrap_or_default();
    if path.len() != length {
        return refuse(format!("{name} device path is truncated"));
    }
    let mut nodes = Vec::new();
    let mut at = 0usize;
    while at.saturating_add(4) <= path.len() {
        let kind = path.get(at).copied().unwrap_or(0);
        let subtype = path.get(at.saturating_add(1)).copied().unwrap_or(0);
        let size = usize::from(u16_at(path, at.saturating_add(2)).unwrap_or(0));
        if size < 4 || at.saturating_add(size) > path.len() {
            return refuse(format!("{name} has malformed device-path node"));
        }
        let data = path
            .get(at.saturating_add(4)..at.saturating_add(size))
            .unwrap_or_default();
        nodes.push(Node {
            kind,
            subtype,
            data,
            size,
        });
        at = at.saturating_add(size);
    }
    let shape: Vec<(u8, u8)> = nodes.iter().map(|n| (n.kind, n.subtype)).collect();
    let [hd, file, end] = nodes.as_slice() else {
        return refuse(format!("{name} is not a single GPT partition and EFI file"));
    };
    if at != path.len() || shape != [(4, 1), (4, 4), (0x7f, 0xff)] {
        return refuse(format!("{name} is not a single GPT partition and EFI file"));
    }
    if hd.size != 42 || end.size != 4 || hd.data.get(36..) != Some(&[2, 2][..]) {
        return refuse(format!("{name} does not name a GPT partition"));
    }
    let partuuid = hd
        .data
        .get(20..36)
        .and_then(guid_le)
        .ok_or_else(|| Refusal(format!("{name} does not name a GPT partition")))?;
    let loader =
        utf16le(file.data).ok_or_else(|| Refusal(format!("{name} EFI path is invalid")))?;
    let loader = loader.trim_end_matches('\0').to_owned();
    if !loader.starts_with('\\') || loader.contains('\0') {
        return refuse(format!("{name} EFI path is malformed"));
    }
    Ok(BootOption {
        description,
        partuuid,
        loader,
    })
}

/// `str.casefold()` for comparing against the ASCII loader paths: lowercase,
/// plus the folds of non-ASCII characters that fold to ASCII letters.
#[must_use]
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ſ' => out.push('s'),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

type State = (Vec<String>, BTreeMap<String, Option<Vec<u8>>>);

/// The firmware facts enlisting requires, and the original bytes of every
/// option involved (to detect a change before and after the mutation).
fn checked_state(
    host: &dyn Host,
    boot_id: &str,
    esp: &str,
    loader: &str,
) -> Result<State, Refusal> {
    let order = ids(host, "BootOrder")?;
    let current = ids(host, "BootCurrent")?;
    let next_boot = ids(host, "BootNext")?;
    let order = match order {
        Some(order) if !order.is_empty() => order,
        _ => return refuse("BootOrder is absent, empty or duplicated"),
    };
    let mut unique = order.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != order.len() {
        return refuse("BootOrder is absent, empty or duplicated");
    }
    if current.as_deref() != Some(&[boot_id.to_owned()][..]) {
        return refuse("Tilo is not the current Boot####; boot Tilo first");
    }
    if next_boot.is_some() {
        return refuse("BootNext is occupied; refusing to change persistent order");
    }
    let first = option(host, order.first().map_or("", String::as_str))?;
    if casefold(&first.loader) != casefold(WINDOWS_LOADER) || first.partuuid == esp {
        return refuse("Windows Boot Manager is not the first BootOrder entry on a separate ESP");
    }
    let mut originals = BTreeMap::new();
    let mut entries = order.clone();
    entries.push(boot_id.to_owned());
    entries.sort_unstable();
    entries.dedup();
    for entry in entries {
        let raw = var(host, &format!("Boot{entry}"), EFI_GLOBAL)?;
        option(host, &entry)?;
        originals.insert(entry, raw);
    }
    let target = option(host, boot_id)?;
    if target.description != "Tilo"
        || target.partuuid != esp
        || casefold(&target.loader) != casefold(loader)
    {
        return refuse("Tilo Boot#### does not match its expected ESP, label and loader");
    }
    let Some(reported) = var(host, "LoaderDevicePartUUID", LOADER_VENDOR)? else {
        return refuse("systemd-boot did not report its loaded ESP");
    };
    let loaded_esp = utf16le(&reported)
        .ok_or_else(|| Refusal("LoaderDevicePartUUID is malformed".to_owned()))?;
    if loaded_esp.trim_end_matches('\0').to_lowercase() != esp {
        return refuse("the running systemd-boot loaded from another ESP");
    }
    Ok((order, originals))
}

/// Appends `boot_id` to `BootOrder` after every check, and reads it back.
/// Returns what to tell the operator.
///
/// # Errors
/// Any refused check, a firmware change between the checks, `efibootmgr`
/// failing, or a readback that differs.
pub fn enlist(host: &dyn Host, boot_id: &str, esp: &str, loader: &str) -> Result<String, Refusal> {
    let (order, originals) = checked_state(host, boot_id, esp, loader)?;
    if order.iter().any(|entry| entry == boot_id) {
        return Ok(format!(
            "Boot{boot_id} is already in BootOrder; Windows remains first"
        ));
    }
    // Detect changes between validation and the only mutation.
    if checked_state(host, boot_id, esp, loader)? != (order.clone(), originals.clone()) {
        return refuse("firmware changed before BootOrder update");
    }
    let mut wanted = order;
    wanted.push(boot_id.to_owned());
    let joined = wanted.join(",");
    let result = host.efibootmgr(&["-q", "-o", &joined])?;
    if !result.status.success() {
        return refuse(format!(
            "efibootmgr failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    // In the original's order, stopping at the first difference.
    let differs = || -> Result<bool, Refusal> {
        if ids(host, "BootOrder")?.as_ref() != Some(&wanted)
            || ids(host, "BootNext")?.is_some()
            || ids(host, "BootCurrent")?.as_deref() != Some(&[boot_id.to_owned()][..])
        {
            return Ok(true);
        }
        for (entry, raw) in &originals {
            if var(host, &format!("Boot{entry}"), EFI_GLOBAL)? != *raw {
                return Ok(true);
            }
        }
        Ok(false)
    };
    if differs()? {
        return refuse("firmware readback differs; inspect efibootmgr -v before further changes");
    }
    Ok(format!(
        "Boot{boot_id} appended to BootOrder; Windows remains first"
    ))
}

fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The command line, without the program name; returns the exit status.
pub fn main(host: &dyn Host, args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let parsed = match parse(args) {
        Ok(Parsed::Run(parsed)) => parsed,
        Ok(Parsed::Help(help)) => {
            let _ = write!(out, "{help}");
            return 0;
        }
        Err(usage) => {
            let _ = write!(err, "{usage}");
            return 2;
        }
    };
    let outcome = (|| {
        if host.euid() != 0 {
            return refuse("run as root after validating Tilo's first boot");
        }
        let boot_ok =
            parsed.boot_id.len() == 4 && parsed.boot_id.bytes().all(|b| b.is_ascii_hexdigit());
        if !is_guid(&parsed.esp_partuuid) || !boot_ok {
            return refuse("expected a canonical ESP GUID and four-digit Boot ID");
        }
        let folded = casefold(&parsed.loader);
        if !LOADERS.iter().any(|path| casefold(path) == folded) {
            return refuse("loader must be the ARM64 systemd-boot primary or fallback path");
        }
        enlist(
            host,
            &parsed.boot_id.to_uppercase(),
            &parsed.esp_partuuid.to_lowercase(),
            &parsed.loader,
        )
    })();
    match outcome {
        Ok(message) => {
            let _ = writeln!(out, "{message}");
            0
        }
        Err(refusal) => {
            let _ = writeln!(err, "tilo-boot-order: {refusal}");
            1
        }
    }
}

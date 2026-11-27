//! `tilo-boot-order`: enlists a successfully booted Tilo after Windows in the
//! firmware boot order without changing the default.
//!
//! ```text
//! tilo-boot-order enlist --esp-partuuid GUID --boot-id XXXX --loader PATH
//! ```
//!
//! Run by hand, as root, after checking the first Tilo boot. It never creates or
//! replaces an EFI boot option: it appends the named, currently booted Tilo
//! option to the end of `BootOrder` once every firmware fact matches, and reads
//! the firmware back afterwards. Exit status 0 when done (or already done), 1 on
//! a refusal, 2 on a usage error.
//!
//! Its sibling `tilo-default-start` ([`default_start`]) makes Tilo the default
//! start after an alongside install, and undoes it.

mod args;

pub mod default_start;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rustix::fs::{CWD, FileType, Mode, OFlags, fstat, openat};
use rustix::io::Errno;
use staging_handoff_model::{efi, is_guid};

pub use args::{Parsed, Request, parse};

pub const EFI_GLOBAL: &str = efi::GLOBAL_GUID;
pub const LOADER_VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
pub const LOADERS: [&str; 2] = [
    r"\EFI\systemd\systemd-bootaa64.efi",
    r"\EFI\BOOT\BOOTAA64.EFI",
];
pub const WINDOWS_LOADER: &str = r"\EFI\Microsoft\Boot\bootmgfw.efi";
const VARIABLE_LIMIT: usize = 65536;

/// Why the command or the operating system refused.
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
pub trait Machine {
    /// The efivarfs mount holding the EFI variables.
    fn efivars(&self) -> PathBuf {
        PathBuf::from("/sys/firmware/efi/efivars")
    }

    fn efibootmgr(&self, args: &[&str]) -> Result<Output, Refusal> {
        Command::new("efibootmgr")
            .args(args)
            .stdin(Stdio::inherit())
            .output()
            .map_err(|error| {
                Refusal(format!(
                    "[Errno {}] {}: 'efibootmgr'",
                    error.raw_os_error().unwrap_or(0),
                    error
                        .to_string()
                        .split(" (os error")
                        .next()
                        .unwrap_or_default()
                ))
            })
    }

    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Machine for System {}

/// The payload of EFI variable `name` (after its four attribute bytes), or
/// `None` when it does not exist.
///
/// # Errors
/// A variable that is not a regular file, truncated or larger than 64 KiB,
/// or one that cannot be opened or read.
pub fn var(machine: &dyn Machine, name: &str, vendor: &str) -> Result<Option<Vec<u8>>, Refusal> {
    let path = machine.efivars().join(format!("{name}-{vendor}"));
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
pub fn ids(machine: &dyn Machine, name: &str) -> Result<Option<Vec<String>>, Refusal> {
    let Some(raw) = var(machine, name, EFI_GLOBAL)? else {
        return Ok(None);
    };
    efi::parse_u16_list(&raw)
        .map(|list| {
            Some(
                list.iter()
                    .map(|number| efi::format_boot_number(*number))
                    .collect(),
            )
        })
        .map_err(|_| Refusal(format!("{name} has odd length")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOption {
    pub description: String,
    pub partuuid: String,
    pub loader: String,
}

/// `Boot####`: active, one GPT hard-drive node, one or more file nodes, the
/// end node.
///
/// # Errors
/// An absent, inactive, truncated or malformed option, or one that does not
/// name exactly one GPT partition and EFI file.
pub fn option(machine: &dyn Machine, boot_id: &str) -> Result<BootOption, Refusal> {
    let name = format!("Boot{boot_id}");
    let raw = match var(machine, &name, EFI_GLOBAL)? {
        Some(raw) if raw.len() >= 10 => raw,
        _ => return refuse(format!("{name} is absent or truncated")),
    };
    let header =
        efi::split_load_option(&raw).map_err(|error| Refusal(format!("{name} {error}")))?;
    if header.attributes & efi::LOAD_OPTION_ACTIVE == 0 {
        return refuse(format!("{name} is inactive"));
    }
    let decoded =
        efi::decode_load_option(&raw).map_err(|error| Refusal(format!("{name} {error}")))?;
    if !decoded.file_path.starts_with('\\') {
        return refuse(format!("{name} EFI path is malformed"));
    }
    Ok(BootOption {
        description: decoded.description,
        partuuid: decoded.partition_guid,
        loader: decoded.file_path,
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
/// option involved, to detect a change between validation and the mutation.
fn checked_state(
    machine: &dyn Machine,
    boot_id: &str,
    esp: &str,
    loader: &str,
) -> Result<State, Refusal> {
    let order = ids(machine, "BootOrder")?;
    let current = ids(machine, "BootCurrent")?;
    let next_boot = ids(machine, "BootNext")?;
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
    let first = option(machine, order.first().map_or("", String::as_str))?;
    if casefold(&first.loader) != casefold(WINDOWS_LOADER) || first.partuuid == esp {
        return refuse("Windows Boot Manager is not the first BootOrder entry on a separate ESP");
    }
    let mut originals = BTreeMap::new();
    let mut entries = order.clone();
    entries.push(boot_id.to_owned());
    entries.sort_unstable();
    entries.dedup();
    for entry in entries {
        let raw = var(machine, &format!("Boot{entry}"), EFI_GLOBAL)?;
        option(machine, &entry)?;
        originals.insert(entry, raw);
    }
    let target = option(machine, boot_id)?;
    if target.description != "Tilo"
        || target.partuuid != esp
        || casefold(&target.loader) != casefold(loader)
    {
        return refuse("Tilo Boot#### does not match its expected ESP, label and loader");
    }
    let Some(reported) = var(machine, "LoaderDevicePartUUID", LOADER_VENDOR)? else {
        return refuse("systemd-boot did not report its loaded ESP");
    };
    let loaded_esp = efi::utf16_text(&reported)
        .map_err(|_| Refusal("LoaderDevicePartUUID is malformed".to_owned()))?;
    if loaded_esp.trim_end_matches('\0').to_lowercase() != esp {
        return refuse("the running systemd-boot loaded from another ESP");
    }
    Ok((order, originals))
}

/// Appends `boot_id` to `BootOrder` after every check, and reads it back.
/// Returns what to tell the operator.
pub fn enlist(
    machine: &dyn Machine,
    boot_id: &str,
    esp: &str,
    loader: &str,
) -> Result<String, Refusal> {
    let (order, originals) = checked_state(machine, boot_id, esp, loader)?;
    if order.iter().any(|entry| entry == boot_id) {
        return Ok(format!(
            "Boot{boot_id} is already in BootOrder; Windows remains first"
        ));
    }
    // Detect changes between validation and the only mutation.
    if checked_state(machine, boot_id, esp, loader)? != (order.clone(), originals.clone()) {
        return refuse("firmware changed before BootOrder update");
    }
    let mut wanted = order;
    wanted.push(boot_id.to_owned());
    let joined = wanted.join(",");
    let result = machine.efibootmgr(&["-q", "-o", &joined])?;
    if !result.status.success() {
        return refuse(format!(
            "efibootmgr failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let differs = || -> Result<bool, Refusal> {
        if ids(machine, "BootOrder")?.as_ref() != Some(&wanted)
            || ids(machine, "BootNext")?.is_some()
            || ids(machine, "BootCurrent")?.as_deref() != Some(&[boot_id.to_owned()][..])
        {
            return Ok(true);
        }
        for (entry, raw) in &originals {
            if var(machine, &format!("Boot{entry}"), EFI_GLOBAL)? != *raw {
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

/// The command line and the exit status.
pub fn main(
    machine: &dyn Machine,
    args: &[OsString],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
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
        if machine.euid() != 0 {
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
            machine,
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

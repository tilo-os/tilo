//! `tilo-staging-handoff`: the live-installer side of USB-free installation
//! (design spec §6).
//!
//! Tilo Imager stages a live installer on the same disk as the retained OS
//! and leaves `/EFI/tilo-staging/handoff.json` on the staging ESP. That file
//! is a *hint, never authority*: every subcommand here re-derives the facts it
//! acts on from firmware variables, the partition table and the mount table,
//! and treats the handoff only as the list of identities those facts must
//! equal.
//!
//! Subcommands (all run as root from the live medium):
//!
//! * `detect --out FILE --error FILE`: before greetd, if the firmware loader
//!   ran from a staging ESP whose handoff names that same ESP, publish a
//!   validated copy for the unprivileged UI.
//! * `check-install --handoff FILE --disk DEV --mode guided|erase|prepared
//!   [...]`: `tilo-install`'s replacement for the live-disk refusal. Prints
//!   `KEY=VALUE` facts on success; any mismatch is a refusal (exit 1).
//! * `check-mounts --handoff FILE --disk DEV`: only the two staging
//!   partitions may be mounted from the target disk.
//! * `write-reclaim-record --handoff FILE --target-var DIR [...]`: record the
//!   identities `tilo-staging-reclaim` needs on the installed system.
//! * `firmware-finalize --handoff FILE --target-esp-partuuid UUID [...]`:
//!   after a successful install, delete the owned staging `Boot####` when its
//!   bytes still point at the staging ESP, then set `BootNext` to the
//!   installed target. `BootOrder` is never written except to undo
//!   efibootmgr's own insertion when it had to create the target entry.
//!
//! It also carries the installer backend's small checks and writes
//! ([`backend_commands`]), so its shell scripts need no interpreter.
//!
//! The firmware variables, the staging ESP, lsblk and efibootmgr are reached
//! through [`Host`]; the shipped binary takes them from the system only. The
//! `staging-handoff-fixture` example takes the old `TILO_STAGING_*` test
//! seams for the loopback proofs.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

use crate::args::{self, Arg, Command};
use crate::py::Dumps;

pub mod backend_commands;
mod retained_commands;
mod user_commands;

const HANDOFF_MAX_BYTES: usize = 64 * 1024;
const HANDOFF_RELATIVE: &str = "EFI/tilo-staging/handoff.json";
const EFI_GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const LOADER_VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// The Tilo staging root type; only the live installer UKI resolves it
/// (`os/usr/libexec/tilo-root-discover`).
const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
const GUIDED_EFI_BYTES: i128 = 2 * 1024 * 1024 * 1024;
const GUIDED_BOOT_BYTES: i128 = 1024 * 1024 * 1024;
const MIN_GUIDED_ROOT_BYTES: i128 = 32 * 1024 * 1024 * 1024;
const MIB: i128 = 1024 * 1024;
const MAX_LBA: i128 = 1 << 62;
const PROFILE_KEYS: [&str; 11] = [
    "schema",
    "hostname",
    "username",
    "full_name",
    "locale",
    "keyboard",
    "timezone",
    "filesystem",
    "prepared_setup",
    "device_label",
    "purpose",
];
const HANDOFF_KEYS: [&str; 11] = [
    "schema",
    "operation_id",
    "host_os",
    "disk_guid",
    "disk_size_bytes",
    "staging_esp",
    "staging_root",
    "target_extent",
    "owned_boot_entry",
    "original_boot_order",
    "profile",
];
const LIVE_MOUNT_PREFIXES: [&str; 3] = ["/sysroot", "/run/initramfs/live", "/run/rootfsbase"];

/// Why a subcommand stopped: a fact that did not match (the caller must leave
/// everything untouched), or an operating-system error. Both exit 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    Refusal(String),
    /// A missing file or device: some callers treat it as "no handoff".
    NotFound(String),
    Os(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refusal(message) | Self::NotFound(message) | Self::Os(message) => {
                f.write_str(message)
            }
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::NotFound(error.to_string())
        } else {
            Self::Os(error.to_string())
        }
    }
}

impl From<rustix::io::Errno> for Fail {
    fn from(error: rustix::io::Errno) -> Self {
        std::io::Error::from(error).into()
    }
}

impl From<crate::Error> for Fail {
    fn from(error: crate::Error) -> Self {
        Self::Os(error.0)
    }
}

/// A subcommand's result.
pub type Result<T> = std::result::Result<T, Fail>;

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(Fail::Refusal(message.into()))
}

/// What a tool left: its status, stdout and stderr as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The live installer's firmware, disks and tools. The provided methods are
/// the production behaviour; the tests and the loopback proofs' fixture
/// example replace them.
pub trait Host {
    /// The efivarfs directory.
    fn efivars(&self) -> PathBuf {
        PathBuf::from("/sys/firmware/efi/efivars")
    }

    /// The efibootmgr program.
    fn efibootmgr(&self) -> String {
        "efibootmgr".into()
    }

    /// Runs a tool to completion, capturing its output.
    ///
    /// # Errors
    /// The tool missing.
    fn run(&self, argv: &[&str]) -> Result<Ran> {
        let output = crate::process::run(argv, None, Duration::MAX)?;
        Ok(Ran {
            code: output.code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// The handoff bytes on the ESP the loader actually ran from.
    ///
    /// # Errors
    /// [`Fail::NotFound`] when the ESP or its handoff is gone; a refusal when
    /// the partition is not an ESP block device.
    fn esp_handoff(&self, esp_partuuid: &str) -> Result<Vec<u8>> {
        let device = format!("/dev/disk/by-partuuid/{esp_partuuid}");
        if !Path::new(&device).exists() {
            return Err(Fail::NotFound(format!(
                "No such file or directory: '{device}'"
            )));
        }
        let device = fs::canonicalize(&device)?;
        let shown = device.to_string_lossy().into_owned();
        if !fs::metadata(&device)?.file_type().is_block_device() {
            return refuse(format!("staging ESP {shown} is not a block device"));
        }
        let part_type = checked(self, &["lsblk", "-dno", "PARTTYPE", &shown])?;
        if crate::py::strip(&part_type).to_lowercase() != ESP_TYPE {
            return refuse(format!(
                "loader partition {shown} is not an EFI System Partition"
            ));
        }
        let mounted = self.run(&["findmnt", "-rn", "-o", "TARGET", "-S", &shown])?;
        let mounted = mounted
            .stdout
            .split('\n')
            .next()
            .unwrap_or_default()
            .to_owned();
        if !mounted.is_empty() {
            return read_bounded(&Path::new(&mounted).join(HANDOFF_RELATIVE));
        }
        // The detect unit may write only below /run/tilo-installer; the mount
        // stays inside that unit's private namespace either way.
        let parent = if Path::new("/run/tilo-installer").is_dir() {
            "/run/tilo-installer"
        } else {
            "/run"
        };
        let directory = tempfile::Builder::new().prefix("esp.").tempdir_in(parent)?;
        let path = directory.path().to_string_lossy().into_owned();
        let result = checked(
            self,
            &[
                "mount",
                "-t",
                "vfat",
                "-o",
                "ro,nosuid,nodev,noexec",
                &shown,
                &path,
            ],
        )
        .and_then(|_| {
            let read = read_bounded(&directory.path().join(HANDOFF_RELATIVE));
            let _ = self.run(&["umount", &path]);
            read
        });
        fs::remove_dir(directory.keep())?;
        result
    }

    /// `lsblk`'s JSON for `disk`.
    ///
    /// # Errors
    /// lsblk failing or printing something other than JSON.
    fn lsblk(&self, disk: &str) -> Result<Value> {
        let output = checked(
            self,
            &[
                "lsblk",
                "-J",
                "-b",
                "-p",
                "-o",
                "NAME,TYPE,START,SIZE,PARTUUID,PARTTYPE,PTTYPE,PTUUID,LOG-SEC,MOUNTPOINTS,UUID,FSTYPE",
                disk,
            ],
        )?;
        serde_json::from_str(&output).map_err(|error| Fail::Os(error.to_string()))
    }
}

/// The live installer as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

/// A tool's stdout, refusing a nonzero exit with its stderr.
fn checked(host: &(impl Host + ?Sized), argv: &[&str]) -> Result<String> {
    let ran = host.run(argv)?;
    if ran.code != Some(0) {
        return refuse(format!(
            "{} failed: {}",
            argv.first().copied().unwrap_or_default(),
            crate::py::strip(&ran.stderr)
        ));
    }
    Ok(ran.stdout)
}

// --- strict handoff parsing ------------------------------------------------

/// A pattern anchored with `^…$`, as Python's `re.match` reads it: `$` also
/// matches before one final newline.
fn dollar(text: &str, pattern: impl Fn(&str) -> bool) -> bool {
    pattern(text) || text.strip_suffix('\n').is_some_and(pattern)
}

fn is_guid_text(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// `GUID_RE.match(text)`.
fn is_guid(text: &str) -> bool {
    dollar(text, is_guid_text)
}

/// `BOOT_ID_RE.match(text)`: four uppercase hex digits.
fn is_boot_id(text: &str) -> bool {
    dollar(text, |text| {
        text.len() == 4 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F'))
    })
}

/// A JSON value whose objects never repeat a key.
struct Strict(Value);

const REPEATED: &str = "\u{0}repeated:";

impl<'de> de::Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Value, E> {
        Ok(serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("{REPEATED}{key}")));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// `repr(text)` of a key, as the refusal names it.
fn repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', "\\'"))
    }
}

fn int(value: &Value, label: &str, minimum: i128) -> Result<i128> {
    let number = value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from));
    match number {
        Some(number) if number >= minimum && number < MAX_LBA => Ok(number),
        _ => refuse(format!("handoff field {label} is not a bounded integer")),
    }
}

fn guid(value: &Value, label: &str) -> Result<String> {
    match value.as_str() {
        Some(text) if is_guid(text) => Ok(text.to_lowercase()),
        _ => refuse(format!("handoff field {label} is not a GUID")),
    }
}

/// An LBA range, with the partition's GUID when it names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    pub partuuid: Option<String>,
    pub first_lba: i128,
    pub last_lba: i128,
}

impl Extent {
    fn json(&self) -> Value {
        let mut map = Map::new();
        if let Some(partuuid) = &self.partuuid {
            map.insert("partuuid".into(), json!(partuuid));
        }
        map.insert("first_lba".into(), json!(self.first_lba));
        map.insert("last_lba".into(), json!(self.last_lba));
        Value::Object(map)
    }

    fn partuuid(&self) -> &str {
        self.partuuid.as_deref().unwrap_or_default()
    }
}

fn extent(value: &Value, label: &str, with_partuuid: bool) -> Result<Extent> {
    let keys: &[&str] = if with_partuuid {
        &["first_lba", "last_lba", "partuuid"]
    } else {
        &["first_lba", "last_lba"]
    };
    let Some(map) = value
        .as_object()
        .filter(|map| map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key)))
    else {
        return refuse(format!("handoff field {label} has unexpected shape"));
    };
    let field = |key: &str| map.get(key).unwrap_or(&Value::Null);
    let first = int(field("first_lba"), &format!("{label}.first_lba"), 1)?;
    let last = int(field("last_lba"), &format!("{label}.last_lba"), 1)?;
    if last < first {
        return refuse(format!("handoff field {label} ends before it starts"));
    }
    let partuuid = if with_partuuid {
        Some(guid(field("partuuid"), &format!("{label}.partuuid"))?)
    } else {
        None
    };
    Ok(Extent {
        partuuid,
        first_lba: first,
        last_lba: last,
    })
}

fn overlaps(a: (i128, i128), b: (i128, i128)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// The normalized handoff (lowercase GUIDs).
#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    pub operation_id: String,
    pub host_os: String,
    pub disk_guid: String,
    pub disk_size_bytes: i128,
    pub staging_esp: Extent,
    pub staging_root: Extent,
    pub target_extent: Extent,
    pub owned_boot_entry: String,
    pub original_boot_order: Vec<String>,
    pub profile: Map<String, Value>,
    pub migration: Option<Value>,
}

impl Handoff {
    /// The handoff as `json.dumps` wrote it.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut value = json!({
            "schema": 1,
            "operation_id": self.operation_id,
            "host_os": self.host_os,
            "disk_guid": self.disk_guid,
            "disk_size_bytes": self.disk_size_bytes,
            "staging_esp": self.staging_esp.json(),
            "staging_root": self.staging_root.json(),
            "target_extent": self.target_extent.json(),
            "owned_boot_entry": self.owned_boot_entry,
            "original_boot_order": self.original_boot_order,
            "profile": self.profile,
        });
        if let Some(migration) = &self.migration
            && let Some(object) = value.as_object_mut()
        {
            object.insert("migration".into(), migration.clone());
        }
        value
    }

    fn same(&self, other: &Self) -> bool {
        let profile = |handoff: &Self| Value::Object(handoff.profile.clone());
        Self {
            profile: Map::new(),
            ..self.clone()
        } == Self {
            profile: Map::new(),
            ..other.clone()
        } && crate::py::equal(&profile(self), &profile(other))
    }
}

/// Validates a parsed handoff.
///
/// # Errors
/// A refusal naming the first field that differs from schema 1.
pub fn validate_handoff(raw: &Value) -> Result<Handoff> {
    let Some(raw) = raw.as_object().filter(|map| {
        (map.len() == HANDOFF_KEYS.len()
            || (map.len() == HANDOFF_KEYS.len().saturating_add(1) && map.contains_key("migration")))
            && HANDOFF_KEYS.iter().all(|key| map.contains_key(*key))
    }) else {
        return refuse("handoff fields differ from schema 1");
    };
    let field = |key: &str| raw.get(key).unwrap_or(&Value::Null);
    if field("schema").as_u64() != Some(1)
        || !(field("schema").is_u64() || field("schema").is_i64())
    {
        return refuse("unsupported handoff schema");
    }
    let host_os = match field("host_os").as_str() {
        Some(os @ ("windows" | "linux")) => os.to_owned(),
        _ => return refuse("handoff names an unsupported host OS"),
    };
    let entry = match field("owned_boot_entry").as_str() {
        Some(entry) if is_boot_id(entry) => entry.to_owned(),
        _ => {
            return refuse("handoff owned_boot_entry is not four uppercase hex digits");
        }
    };
    let order: Vec<String> = match field("original_boot_order").as_array() {
        Some(items)
            if items.len() <= 128
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(is_boot_id)) =>
        {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        }
        _ => return refuse("handoff original_boot_order is malformed"),
    };
    if order.contains(&entry) {
        return refuse("the owned staging entry was part of the original BootOrder");
    }
    let Some(profile) = field("profile").as_object().filter(|profile| {
        profile
            .keys()
            .all(|key| PROFILE_KEYS.contains(&key.as_str()))
    }) else {
        return refuse("handoff profile carries fields outside the installer profile");
    };
    validate_profile(profile)?;
    let mut handoff = Handoff {
        operation_id: guid(field("operation_id"), "operation_id")?,
        host_os,
        disk_guid: guid(field("disk_guid"), "disk_guid")?,
        disk_size_bytes: int(field("disk_size_bytes"), "disk_size_bytes", 1)?,
        staging_esp: extent(field("staging_esp"), "staging_esp", true)?,
        staging_root: extent(field("staging_root"), "staging_root", true)?,
        target_extent: extent(field("target_extent"), "target_extent", false)?,
        owned_boot_entry: entry,
        original_boot_order: order,
        profile: profile.clone(),
        migration: None,
    };
    let (esp, root, target) = (
        &handoff.staging_esp,
        &handoff.staging_root,
        &handoff.target_extent,
    );
    if esp.partuuid == root.partuuid
        || [esp.partuuid(), root.partuuid()].contains(&handoff.disk_guid.as_str())
    {
        return refuse("handoff GUIDs are not distinct");
    }
    let span = |extent: &Extent| (extent.first_lba, extent.last_lba);
    for (a, b, label) in [
        (esp, root, "staging partitions"),
        (target, esp, "target and staging ESP"),
        (target, root, "target and staging root"),
    ] {
        if overlaps(span(a), span(b)) {
            return refuse(format!("handoff {label} overlap"));
        }
    }
    if let Some(raw_migration) = raw.get("migration") {
        let migration: staging_handoff_model::Migration =
            serde_json::from_value(raw_migration.clone())
                .map_err(|e| Fail::Refusal(e.to_string()))?;
        let typed: staging_handoff_model::Handoff =
            serde_json::from_value(handoff.json()).map_err(|e| Fail::Refusal(e.to_string()))?;
        migration
            .validate_in_handoff(&typed)
            .map_err(Fail::Refusal)?;
        handoff.migration = Some(raw_migration.clone());
    }
    Ok(handoff)
}

fn validate_profile(profile: &Map<String, Value>) -> Result<()> {
    for (key, value) in profile {
        if matches!(key.as_str(), "prepared_setup" | "device_label" | "purpose") {
            continue;
        }
        if key == "schema" {
            if !(value.is_i64() || value.is_u64()) {
                return refuse("handoff profile schema is not an integer");
            }
        } else if value.as_str().is_none_or(|text| text.chars().count() > 256) {
            return refuse(format!(
                "handoff profile field {key} is not a bounded string"
            ));
        }
    }
    staging_handoff_model::validate_profile(profile).map_err(Fail::Refusal)
}

fn validate_migration(raw: &Value) -> Result<Value> {
    let migration: staging_handoff_model::Migration =
        serde_json::from_value(raw.clone()).map_err(|e| Fail::Refusal(e.to_string()))?;
    migration.validate_standalone().map_err(Fail::Refusal)?;
    Ok(raw.clone())
}

/// Parses and validates handoff bytes.
///
/// # Errors
/// Oversized, not UTF-8, not JSON, a repeated field, or invalid.
pub fn parse_handoff(data: &[u8]) -> Result<Handoff> {
    if data.len() > HANDOFF_MAX_BYTES {
        return refuse(format!("handoff is larger than {HANDOFF_MAX_BYTES} bytes"));
    }
    let text = std::str::from_utf8(data)
        .map_err(|error| Fail::Refusal(format!("handoff is not valid JSON: {error}")))?;
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let parsed = <Strict as de::Deserialize>::deserialize(&mut deserializer)
        .and_then(|value| deserializer.end().map(|()| value));
    match parsed {
        Ok(Strict(raw)) => validate_handoff(&raw),
        Err(error) => {
            let message = error.to_string();
            if let Some(key) = message
                .strip_prefix(REPEATED)
                .map(|rest| rest.split(" at line ").next().unwrap_or(rest))
            {
                return refuse(format!("handoff repeats the field {}", repr(key)));
            }
            refuse(format!("handoff is not valid JSON: {message}"))
        }
    }
}

/// A regular file of at most 64 KiB, never through a link.
///
/// # Errors
/// [`Fail::NotFound`] when absent; a refusal when not regular or too large.
pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut file = fs::File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return refuse(format!("{} is not a regular file", path.display()));
    }
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(chunk.get(..count).unwrap_or_default());
        if data.len() > HANDOFF_MAX_BYTES {
            return refuse(format!(
                "{} is larger than {HANDOFF_MAX_BYTES} bytes",
                path.display()
            ));
        }
    }
    Ok(data)
}

fn load_handoff_file(path: &Path) -> Result<Handoff> {
    parse_handoff(&read_bounded(path)?)
}

// --- firmware variables ------------------------------------------------------

fn read_efivar(host: &dyn Host, name: &str, vendor: &str) -> Result<Option<Vec<u8>>> {
    match read_bounded(&host.efivars().join(format!("{name}-{vendor}"))) {
        Ok(data) => match data.get(4..) {
            Some(value) => Ok(Some(value.to_vec())),
            None => refuse(format!("firmware variable {name} is truncated")),
        },
        Err(Fail::NotFound(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

fn utf16(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            u16::from_le_bytes([
                pair.first().copied().unwrap_or(0),
                pair.get(1).copied().unwrap_or(0),
            ])
        })
        .collect();
    String::from_utf16(&units).ok()
}

/// The GUID of the partition the firmware loaded the installer from.
fn loader_device_partuuid(host: &dyn Host) -> Result<Option<String>> {
    let Some(value) = read_efivar(host, "LoaderDevicePartUUID", LOADER_VENDOR)? else {
        return Ok(None);
    };
    let Some(text) = utf16(&value) else {
        return refuse("LoaderDevicePartUUID is not UTF-16");
    };
    let text = crate::py::strip(text.trim_end_matches('\0'));
    if !is_guid(text) {
        return refuse("LoaderDevicePartUUID is not a GUID");
    }
    Ok(Some(text.to_lowercase()))
}

fn boot_ids(host: &dyn Host, name: &str) -> Result<Vec<String>> {
    let Some(value) = read_efivar(host, name, EFI_GLOBAL)? else {
        return Ok(Vec::new());
    };
    if !value.len().is_multiple_of(2) {
        return refuse(format!("{name} has an odd length"));
    }
    Ok(value
        .chunks_exact(2)
        .map(|pair| {
            format!(
                "{:04X}",
                u16::from_le_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0)
                ])
            )
        })
        .collect())
}

/// A load option's single GPT hard-drive node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEntry {
    pub partition_number: u32,
    pub start_lba: u64,
    pub size_lba: u64,
    pub partuuid: String,
    pub path: Option<String>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    let bytes = data.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// `str(uuid.UUID(bytes_le=signature))`.
fn uuid_le(signature: &[u8]) -> Option<String> {
    let b = |at: usize| signature.get(at).copied();
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    let mut text = String::new();
    for (index, at) in order.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _ = write!(text, "{:02x}", b(*at)?);
    }
    Some(text)
}

/// Decodes an `EFI_LOAD_OPTION` (without efivarfs attributes) into its single
/// GPT hard-drive node. Anything else (MBR, several HD nodes, a truncated
/// path) is not a staging entry and decodes to `None`.
#[must_use]
pub fn decode_load_option(data: &[u8]) -> Option<BootEntry> {
    if data.len() < 6 {
        return None;
    }
    let path_length = usize::from(u16_at(data, 4)?);
    let mut offset = 6usize;
    loop {
        if offset.checked_add(2)? > data.len() {
            return None;
        }
        if data.get(offset..offset.checked_add(2)?)? == [0, 0] {
            offset = offset.checked_add(2)?;
            break;
        }
        offset = offset.checked_add(2)?;
    }
    let path = data.get(offset..)?;
    let path = path.get(..path_length)?;
    let mut hd = Vec::new();
    let mut file_path = None;
    let mut cursor = 0usize;
    while cursor.checked_add(4)? <= path.len() {
        let kind = *path.get(cursor)?;
        let subtype = *path.get(cursor.checked_add(1)?)?;
        let length = usize::from(u16_at(path, cursor.checked_add(2)?)?);
        if length < 4 || cursor.checked_add(length)? > path.len() {
            return None;
        }
        let body = path.get(cursor.checked_add(4)?..cursor.checked_add(length)?)?;
        if kind == 0x7F {
            break;
        }
        if kind == 0x04 && subtype == 0x01 {
            if length != 42 {
                return None;
            }
            let (number, start, size) = (u32_at(body, 0)?, u64_at(body, 4)?, u64_at(body, 12)?);
            let (signature, mbr_type, signature_type) =
                (body.get(20..36)?, *body.get(36)?, *body.get(37)?);
            if mbr_type != 0x02 || signature_type != 0x02 {
                return None;
            }
            hd.push(BootEntry {
                partition_number: number,
                start_lba: start,
                size_lba: size,
                partuuid: uuid_le(signature)?,
                path: None,
            });
        } else if kind == 0x04 && subtype == 0x04 {
            file_path = Some(utf16(body)?.trim_end_matches('\0').to_owned());
        }
        cursor = cursor.checked_add(length)?;
    }
    let [entry] = <[BootEntry; 1]>::try_from(hd).ok()?;
    Some(BootEntry {
        path: file_path,
        ..entry
    })
}

fn read_boot_entry(host: &dyn Host, entry_id: &str) -> Result<Option<BootEntry>> {
    Ok(read_efivar(host, &format!("Boot{entry_id}"), EFI_GLOBAL)?
        .and_then(|data| decode_load_option(&data)))
}

fn all_boot_entries(host: &dyn Host) -> Result<Vec<(String, Option<BootEntry>)>> {
    let mut names: Vec<String> = match fs::read_dir(host.efivars()) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<_>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    names.sort();
    let suffix = format!("-{EFI_GLOBAL}");
    let mut entries: Vec<(String, Option<BootEntry>)> = Vec::new();
    for name in names {
        let Some(id) = name
            .strip_prefix("Boot")
            .and_then(|rest| {
                dollar(rest, |rest| {
                    rest.ends_with(&suffix) && rest.len() == suffix.len().saturating_add(4)
                })
                .then_some(rest)
            })
            .and_then(|rest| rest.get(..4))
            .filter(|id| id.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        let entry = read_boot_entry(host, id)?;
        let key = id.to_uppercase();
        if let Some(slot) = entries.iter_mut().find(|(existing, _)| *existing == key) {
            slot.1 = entry;
        } else {
            entries.push((key, entry));
        }
    }
    Ok(entries)
}

// --- the live handoff ----------------------------------------------------

/// The UI's copy must equal what the firmware-booted staging ESP holds.
fn verified_live_handoff(host: &dyn Host, copy_path: &Path) -> Result<Handoff> {
    let copy = load_handoff_file(copy_path)?;
    let Some(loader) = loader_device_partuuid(host)? else {
        return refuse("firmware did not report the ESP the installer booted from");
    };
    if loader != copy.staging_esp.partuuid() {
        return refuse("the handoff names a different ESP than the one this installer booted from");
    }
    let original = match host.esp_handoff(&loader) {
        Ok(data) => parse_handoff(&data)?,
        Err(Fail::NotFound(_)) => {
            return refuse("the booted staging ESP no longer holds a handoff");
        }
        Err(other) => return Err(other),
    };
    if !original.same(&copy) {
        return refuse("the handoff copy differs from the staging ESP");
    }
    Ok(copy)
}

// --- partition facts ---------------------------------------------------------

/// `str(value or '')` for the identities lsblk and sfdisk report.
fn or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(value) if !crate::py::truthy(value) => String::new(),
        Some(value) => python_str(value),
    }
}

/// `str(value)` of a JSON value.
fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `'%s' % node.get(key)`.
fn shown(node: &Map<String, Value>, key: &str) -> String {
    python_str(node.get(key).unwrap_or(&Value::Null))
}

fn lsblk_disk(host: &dyn Host, disk: &str) -> Result<Map<String, Value>> {
    let data = host.lsblk(disk)?;
    let empty = Vec::new();
    let devices: Vec<&Map<String, Value>> = data
        .as_object()
        .and_then(|data| data.get("blockdevices"))
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(Value::as_object)
        .filter(|item| item.get("name").and_then(Value::as_str) == Some(disk))
        .collect();
    match devices.as_slice() {
        [device] if device.get("type").and_then(Value::as_str) == Some("disk") => {
            with_gpt_facts(host, (*device).clone(), disk)
        }
        _ => refuse(format!("{disk} is not exactly one whole disk")),
    }
}

fn children(node: &Map<String, Value>) -> Vec<Map<String, Value>> {
    node.get("children")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_object).cloned().collect())
        .unwrap_or_default()
}

/// Fills GPT identities lsblk left empty (no udev database) from sfdisk.
///
/// lsblk takes PTTYPE/PTUUID/PARTUUID/PARTTYPE from udev; without a udev
/// database (early boot, containers) they come back empty. sfdisk reads the
/// same facts from the GPT itself. Only missing fields are filled; a field
/// both tools report must agree, or the disk is refused.
fn with_gpt_facts(
    host: &dyn Host,
    mut node: Map<String, Value>,
    disk: &str,
) -> Result<Map<String, Value>> {
    let mut kids = children(&node);
    let present = |map: &Map<String, Value>, key: &str| map.get(key).is_some_and(crate::py::truthy);
    let complete = present(&node, "pttype")
        && present(&node, "ptuuid")
        && kids
            .iter()
            .all(|child| present(child, "partuuid") && present(child, "parttype"));
    if complete {
        return Ok(node);
    }
    let output = checked(host, &["sfdisk", "--json", disk])?;
    let parsed: Value =
        serde_json::from_str(&output).map_err(|error| Fail::Os(error.to_string()))?;
    let table = parsed
        .as_object()
        .and_then(|parsed| parsed.get("partitiontable"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let fill = |target: &mut Map<String, Value>, key: &str, value: Option<&Value>| -> Result<()> {
        let value = or_empty(value).to_lowercase();
        let have = or_empty(target.get(key)).to_lowercase();
        if !have.is_empty() && !value.is_empty() && have != value {
            return refuse(format!(
                "lsblk and the GPT disagree about {key} of {}",
                shown(target, "name")
            ));
        }
        if have.is_empty() {
            target.insert(key.into(), Value::String(value));
        }
        Ok(())
    };
    fill(&mut node, "pttype", table.get("label"))?;
    fill(&mut node, "ptuuid", table.get("id"))?;
    let partitions: Vec<Map<String, Value>> = table
        .get("partitions")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_object).cloned().collect())
        .unwrap_or_default();
    for child in &mut kids {
        let name = child.get("name").cloned().unwrap_or(Value::Null);
        // `{part.get('node'): part}`: the last partition of a node wins.
        let Some(part) = partitions
            .iter()
            .rev()
            .find(|part| part.get("node").unwrap_or(&Value::Null) == &name)
        else {
            return refuse(format!(
                "the GPT of {disk} does not list {}",
                python_str(&name)
            ));
        };
        fill(child, "partuuid", part.get("uuid"))?;
        fill(child, "parttype", part.get("type"))?;
    }
    if node.contains_key("children") {
        node.insert(
            "children".into(),
            Value::Array(kids.into_iter().map(Value::Object).collect()),
        );
    }
    Ok(node)
}

fn number(node: &Map<String, Value>, key: &str) -> Result<i128> {
    let value = node.get(key).unwrap_or(&Value::Null);
    let parsed = match value {
        Value::String(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            text.parse::<i128>().ok()
        }
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    };
    match parsed {
        Some(number) if number >= 0 => Ok(number),
        _ => refuse(format!(
            "lsblk reported no {key} for {}",
            shown(node, "name")
        )),
    }
}

fn mounts(node: &Map<String, Value>) -> Vec<String> {
    let points = match node.get("mountpoints") {
        None | Some(Value::Null) => vec![node.get("mountpoint").cloned().unwrap_or(Value::Null)],
        Some(Value::Array(points)) => points.clone(),
        Some(other) => vec![other.clone()],
    };
    points
        .iter()
        .filter(|point| crate::py::truthy(point))
        .map(python_str)
        .collect()
}

fn subtree_mounts(node: &Map<String, Value>) -> Vec<String> {
    let mut found = mounts(node);
    for child in children(node) {
        found.extend(subtree_mounts(&child));
    }
    found
}

fn is_live_mount(point: &str) -> bool {
    point == "/"
        || point == "/boot"
        || point.starts_with("/boot/")
        || LIVE_MOUNT_PREFIXES
            .iter()
            .any(|prefix| point == *prefix || point.starts_with(&format!("{prefix}/")))
}

/// One partition of the target disk, as lsblk and the GPT report it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Partition {
    name: String,
    partuuid: String,
    kind: String,
    first_lba: i128,
    last_lba: i128,
    mounts: Vec<String>,
}

/// Validates the target disk against the handoff; returns its sector size and
/// partitions.
fn disk_facts(host: &dyn Host, disk: &str, handoff: &Handoff) -> Result<(i128, Vec<Partition>)> {
    let node = lsblk_disk(host, disk)?;
    let lower = |key: &str| {
        node.get(key)
            .map_or_else(String::new, python_str)
            .to_lowercase()
    };
    if lower("pttype") != "gpt" {
        return refuse(format!("{disk} is not a GPT disk"));
    }
    if lower("ptuuid") != handoff.disk_guid {
        return refuse("disk GUID differs from the staged plan");
    }
    if number(&node, "size")? != handoff.disk_size_bytes {
        return refuse("disk size differs from the staged plan");
    }
    let sector = number(&node, "log-sec")?;
    if sector != 512 && sector != 4096 {
        return refuse(format!("unsupported logical sector size {sector}"));
    }
    if !mounts(&node).is_empty() {
        return refuse(format!("the whole disk {disk} is mounted"));
    }
    let mut partitions = Vec::new();
    for child in children(&node) {
        if child.get("type").and_then(Value::as_str) != Some("part") {
            return refuse(format!(
                "unexpected {} node below {disk}",
                shown(&child, "type")
            ));
        }
        let start_bytes = number(&child, "start")?.saturating_mul(512);
        let size = number(&child, "size")?;
        if start_bytes.checked_rem(sector) != Some(0)
            || size.checked_rem(sector) != Some(0)
            || size == 0
        {
            return refuse(format!(
                "partition {} is not sector aligned",
                shown(&child, "name")
            ));
        }
        let first = start_bytes.checked_div(sector).unwrap_or_default();
        partitions.push(Partition {
            name: shown(&child, "name"),
            partuuid: or_empty(child.get("partuuid")).to_lowercase(),
            kind: or_empty(child.get("parttype")).to_lowercase(),
            first_lba: first,
            last_lba: first
                .saturating_add(size.checked_div(sector).unwrap_or_default())
                .saturating_sub(1),
            mounts: subtree_mounts(&child),
        });
    }
    validate_source_partition(&node, handoff, sector)?;
    Ok((sector, partitions))
}

fn validate_source_partition(
    node: &Map<String, Value>,
    handoff: &Handoff,
    sector: i128,
) -> Result<()> {
    if let Some(migration) = &handoff.migration {
        let source = migration
            .get("home")
            .and_then(|home| home.get("volume"))
            .unwrap_or(&Value::Null);
        let matches: Vec<_> = children(node)
            .into_iter()
            .filter(|part| {
                or_empty(part.get("partuuid")).eq_ignore_ascii_case(
                    source
                        .get("partition_guid")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or_default(),
                )
            })
            .collect();
        let [part] = matches.as_slice() else {
            return refuse("retained migration source identity or geometry changed");
        };
        let first = int(
            source.get("first_lba").unwrap_or(&Value::Null),
            "source first LBA",
            1,
        )?;
        let last = int(
            source.get("last_lba").unwrap_or(&Value::Null),
            "source last LBA",
            1,
        )?;
        if int(
            source.get("sector_bytes").unwrap_or(&Value::Null),
            "source sector",
            1,
        )? != sector
            || number(part, "start")?.checked_mul(512) != first.checked_mul(sector)
            || Some(number(part, "size")?)
                != last
                    .checked_sub(first)
                    .and_then(|n| n.checked_add(1))
                    .and_then(|n| n.checked_mul(sector))
            || !or_empty(part.get("uuid")).eq_ignore_ascii_case(
                source
                    .get("filesystem_uuid")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or_default(),
            )
            || or_empty(part.get("fstype")).to_lowercase()
                != source
                    .get("filesystem")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or_default()
        {
            return refuse("retained migration source identity or geometry changed");
        }
    }
    Ok(())
}

fn staged_partition<'a>(
    partitions: &'a [Partition],
    expected: &Extent,
    part_type: &str,
    label: &str,
) -> Result<&'a Partition> {
    let matches: Vec<&Partition> = partitions
        .iter()
        .filter(|part| part.partuuid == expected.partuuid())
        .collect();
    let [part] = matches.as_slice() else {
        return refuse(format!(
            "{label} {} is not present exactly once",
            expected.partuuid()
        ));
    };
    if part.kind != part_type {
        return refuse(format!("{label} has an unexpected partition type"));
    }
    if (part.first_lba, part.last_lba) != (expected.first_lba, expected.last_lba) {
        return refuse(format!("{label} extent differs from the staged plan"));
    }
    Ok(part)
}

fn check_mounts(
    handoff: &Handoff,
    partitions: &[Partition],
    staging_root: &Partition,
) -> Result<()> {
    let staged = [
        handoff.staging_esp.partuuid(),
        handoff.staging_root.partuuid(),
    ];
    for part in partitions {
        if !part.mounts.is_empty() && !staged.contains(&part.partuuid.as_str()) {
            return refuse(format!(
                "{} is mounted; only the staged installer may be in use",
                part.name
            ));
        }
    }
    if !staging_root.mounts.iter().any(|point| is_live_mount(point)) {
        return refuse("the running installer root is not the staging root partition");
    }
    Ok(())
}

/// The guided plan `check-install` checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Plan {
    pub free_start_bytes: i128,
    pub free_size_bytes: i128,
    pub efi_start_bytes: i128,
    pub efi_size_bytes: i128,
    pub boot_start_bytes: i128,
    pub boot_size_bytes: i128,
    pub root_start_bytes: i128,
    pub root_size_bytes: i128,
    pub sector_size: i128,
}

/// `check-install`: the facts `tilo-install` needs, one `KEY=VALUE` per line.
///
/// # Errors
/// Any fact that differs from the staged plan.
pub fn check_install(
    host: &dyn Host,
    handoff_path: &Path,
    disk: &str,
    mode: &str,
    plan: &Plan,
) -> Result<String> {
    if mode != "guided" {
        return refuse(
            "the disk holding the running installer accepts only guided alongside; \
             whole-disk erase and prepared partitions are refused",
        );
    }
    let handoff = verified_live_handoff(host, handoff_path)?;
    let (sector, partitions) = disk_facts(host, disk, &handoff)?;
    if plan.sector_size != sector {
        return refuse("reviewed logical sector size differs from the disk");
    }
    let esp = staged_partition(&partitions, &handoff.staging_esp, ESP_TYPE, "staging ESP")?;
    let root = staged_partition(
        &partitions,
        &handoff.staging_root,
        STAGING_ROOT_TYPE,
        "staging root",
    )?;
    if Some(esp.partuuid.as_str()) != loader_device_partuuid(host)?.as_deref() {
        return refuse("the installer did not boot from the staging ESP");
    }
    check_mounts(&handoff, &partitions, root)?;
    let target = &handoff.target_extent;
    let free_start = target.first_lba.saturating_mul(sector);
    let free_size = target
        .last_lba
        .saturating_sub(target.first_lba)
        .saturating_add(1)
        .saturating_mul(sector);
    if (plan.free_start_bytes, plan.free_size_bytes) != (free_start, free_size) {
        return refuse("the guided extent differs from the staged target extent");
    }
    if free_start % MIB != 0 || free_size % MIB != 0 {
        return refuse("the staged target extent is not MiB aligned");
    }
    if free_start.saturating_add(free_size) > handoff.disk_size_bytes.saturating_sub(MIB) {
        return refuse("the staged target extent reaches the backup GPT");
    }
    for part in &partitions {
        if overlaps(
            (part.first_lba, part.last_lba),
            (target.first_lba, target.last_lba),
        ) {
            return refuse(format!(
                "the staged target extent is no longer unallocated ({})",
                part.name
            ));
        }
    }
    if plan.efi_start_bytes != free_start || plan.efi_size_bytes != GUIDED_EFI_BYTES {
        return refuse("the Tilo ESP must be 2 GiB at the start of the target extent");
    }
    if plan.boot_start_bytes != free_start.saturating_add(GUIDED_EFI_BYTES)
        || plan.boot_size_bytes != GUIDED_BOOT_BYTES
    {
        return refuse("the Tilo boot must be 1 GiB immediately after the Tilo ESP");
    }
    if plan.root_start_bytes != plan.boot_start_bytes.saturating_add(GUIDED_BOOT_BYTES) {
        return refuse("the Tilo root must immediately follow the Tilo boot");
    }
    if plan.root_start_bytes.saturating_add(plan.root_size_bytes)
        != free_start.saturating_add(free_size)
    {
        return refuse("the Tilo root must end exactly where the staged installer begins");
    }
    if plan.root_size_bytes < MIN_GUIDED_ROOT_BYTES {
        return refuse("the staged target extent is smaller than the guided minimum");
    }
    // Sorted by key, as the script printed them.
    Ok(format!(
        "INTERNAL_DISK_GUID={}\nINTERNAL_HOST_OS={}\nINTERNAL_OWNED_BOOT_ENTRY={}\n\
         INTERNAL_STAGING_ESP_PARTUUID={}\nINTERNAL_STAGING_ROOT_PARTUUID={}\n",
        handoff.disk_guid, handoff.host_os, handoff.owned_boot_entry, esp.partuuid, root.partuuid
    ))
}

/// `check-mounts`: only the staging partitions are in use.
///
/// # Errors
/// A staging partition missing or changed, or anything else mounted.
pub fn check_mounts_command(host: &dyn Host, handoff_path: &Path, disk: &str) -> Result<()> {
    let handoff = load_handoff_file(handoff_path)?;
    let (_, partitions) = disk_facts(host, disk, &handoff)?;
    let root = staged_partition(
        &partitions,
        &handoff.staging_root,
        STAGING_ROOT_TYPE,
        "staging root",
    )?;
    staged_partition(&partitions, &handoff.staging_esp, ESP_TYPE, "staging ESP")?;
    check_mounts(&handoff, &partitions, root)
}

// --- publishing --------------------------------------------------------------

/// Replaces `path` with `data` (mode `mode`) through a synced temporary file.
fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let mut temporary = tempfile::Builder::new()
        .prefix(".tilo-staging.")
        .tempfile_in(&directory)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Fail::from(error.error))?;
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

fn canonical(document: &Value) -> Vec<u8> {
    let mut text = Dumps::CANONICAL.dumps(document);
    text.push('\n');
    text.into_bytes()
}

/// What `detect` printed and published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Printed {
    pub stdout: String,
    pub stderr: String,
}

/// `detect`: publishes a validated copy of the booted staging ESP's handoff.
///
/// # Errors
/// Only an operating-system error; a mismatch is published to `error`.
pub fn detect(host: &dyn Host, out: &Path, error: &Path) -> Result<Printed> {
    for stale in [out, error] {
        match fs::remove_file(stale) {
            Err(problem) if problem.kind() != std::io::ErrorKind::NotFound => {
                return Err(problem.into());
            }
            _ => {}
        }
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(out.parent().unwrap_or_else(|| Path::new("")))?;
    if !host.efivars().is_dir() {
        return Ok(Printed::default());
    }
    let recognised = (|| -> Result<Option<Handoff>> {
        let Some(loader) = loader_device_partuuid(host)? else {
            return Ok(None);
        };
        let handoff = match host.esp_handoff(&loader) {
            Ok(data) => parse_handoff(&data)?,
            Err(Fail::NotFound(_)) => return Ok(None),
            Err(other) => return Err(other),
        };
        if handoff.staging_esp.partuuid() != loader {
            return refuse(
                "the staging handoff names a different ESP than the one this installer booted from",
            );
        }
        Ok(Some(handoff))
    })();
    match recognised {
        Ok(None) => Ok(Printed::default()),
        Err(Fail::Refusal(reason)) => {
            write_atomic(
                error,
                format!("Staged installation was not recognised: {reason}\n").as_bytes(),
                0o644,
            )?;
            Ok(Printed {
                stdout: String::new(),
                stderr: format!("tilo-staging-handoff: {reason}\n"),
            })
        }
        Err(other) => Err(other),
        Ok(Some(handoff)) => {
            write_atomic(out, &canonical(&handoff.json()), 0o644)?;
            Ok(Printed {
                stdout: format!(
                    "tilo-staging-handoff: staged installation for disk {} recognised\n",
                    handoff.disk_guid
                ),
                stderr: String::new(),
            })
        }
    }
}

/// The installed root `write-reclaim-record` records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRoot {
    pub root_partuuid: String,
    pub esp_partuuid: String,
    pub root_start_bytes: i128,
    pub root_size_bytes: i128,
    pub sector_size: i128,
}

/// `write-reclaim-record`: what `tilo-staging-reclaim` needs, in the
/// installed `/var`.
///
/// # Errors
/// Malformed GUIDs or geometry, a root that does not end at the staged
/// extent, or a write failing.
pub fn write_reclaim_record(
    handoff_path: &Path,
    target_var: &Path,
    target: &TargetRoot,
) -> Result<String> {
    let handoff = load_handoff_file(handoff_path)?;
    if !is_guid(&target.root_partuuid) || !is_guid(&target.esp_partuuid) {
        return refuse("target partition GUIDs are malformed");
    }
    let sector = target.sector_size;
    if (sector != 512 && sector != 4096)
        || target.root_start_bytes.rem_euclid(sector) != 0
        || target.root_size_bytes.rem_euclid(sector) != 0
    {
        return refuse("target root geometry is not sector aligned");
    }
    let first = target.root_start_bytes.div_euclid(sector);
    let last = first
        .saturating_add(target.root_size_bytes.div_euclid(sector))
        .saturating_sub(1);
    if last != handoff.target_extent.last_lba || first < handoff.target_extent.first_lba {
        return refuse("target root does not end at the staged target extent");
    }
    let record = json!({
        "schema": 1,
        "operation_id": handoff.operation_id,
        "disk_guid": handoff.disk_guid,
        "disk_size_bytes": handoff.disk_size_bytes,
        "sector_bytes": sector,
        "staging_esp": handoff.staging_esp.json(),
        "staging_root": handoff.staging_root.json(),
        "target_root": {
            "partuuid": target.root_partuuid.to_lowercase(),
            "first_lba": first,
            "last_lba": last,
        },
        "target_esp_partuuid": target.esp_partuuid.to_lowercase(),
    });
    let directory = target_var.join("lib").join("tilo");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&directory)?;
    write_atomic(
        &directory.join("staging-reclaim.json"),
        &canonical(&record),
        0o600,
    )?;
    // Preserve every migration policy, including legacy copy intent, for first boot.
    // Use the same private, no-clobber publication as authenticated retained delivery.
    retained_commands::deliver_retained(
        handoff.migration.as_ref(),
        target_var,
        validate_migration,
    )?;
    Ok("recorded staging reclaim for the installed system\n".into())
}

// --- firmware handoff back -----------------------------------------------

fn efibootmgr_run(host: &dyn Host, argv: &[&str]) -> Result<()> {
    let program = host.efibootmgr();
    let mut full = vec![program.as_str(), "-q"];
    full.extend_from_slice(argv);
    let ran = host.run(&full)?;
    if ran.code != Some(0) {
        return refuse(format!(
            "efibootmgr {} failed: {}",
            argv.join(" "),
            crate::py::strip(&ran.stderr)
        ));
    }
    Ok(())
}

fn delete_owned_entry(host: &dyn Host, handoff: &Handoff, out: &mut String) -> Result<bool> {
    let entry_id = &handoff.owned_boot_entry;
    let esp = &handoff.staging_esp;
    let path = host.efivars().join(format!("Boot{entry_id}-{EFI_GLOBAL}"));
    if !path.exists() {
        let _ = writeln!(out, "staging boot entry Boot{entry_id} is already gone");
        return Ok(true);
    }
    let entry = read_boot_entry(host, entry_id)?;
    let points_at_staging = entry.is_some_and(|entry| {
        entry.partuuid == esp.partuuid()
            && i128::from(entry.start_lba) == esp.first_lba
            && i128::from(entry.size_lba)
                == esp.last_lba.saturating_sub(esp.first_lba).saturating_add(1)
    });
    if !points_at_staging {
        let _ = writeln!(
            out,
            "warning: Boot{entry_id} no longer points at the staging ESP; leaving it untouched"
        );
        return Ok(false);
    }
    if boot_ids(host, "BootOrder")?.contains(entry_id) {
        let _ = writeln!(
            out,
            "warning: Boot{entry_id} is referenced by BootOrder; leaving it for Tilo Imager to remove"
        );
        return Ok(false);
    }
    efibootmgr_run(host, &["-b", entry_id, "-B"])?;
    if path.exists() {
        return refuse(format!("Boot{entry_id} still exists after deletion"));
    }
    let _ = writeln!(out, "removed the staging boot entry Boot{entry_id}");
    Ok(true)
}

fn target_entries(host: &dyn Host, target_esp: &str) -> Result<Vec<String>> {
    let mut ids: Vec<String> = all_boot_entries(host)?
        .into_iter()
        .filter(|(_, entry)| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.partuuid == target_esp)
        })
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    Ok(ids)
}

/// The installed target `firmware-finalize` points the next restart at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finalize {
    pub target_esp_partuuid: String,
    pub target_disk: String,
    pub target_esp_partition_number: i128,
    pub loader: String,
}

/// `firmware-finalize`: removes the owned staging entry when it is still
/// ours, and sets `BootNext` to the installed Tilo once. Returns the exit
/// status (0, or 3 when the staging entry was left in place) and what it
/// printed.
///
/// # Errors
/// A malformed target, an ambiguous or absent target entry, efibootmgr
/// failing, or a variable that did not persist.
pub fn firmware_finalize(
    host: &dyn Host,
    handoff_path: &Path,
    request: &Finalize,
    out: &mut String,
) -> Result<u8> {
    let handoff = load_handoff_file(handoff_path)?;
    let target_esp = request.target_esp_partuuid.to_lowercase();
    if !is_guid(&target_esp)
        || target_esp == handoff.staging_esp.partuuid()
        || target_esp == handoff.staging_root.partuuid()
    {
        return refuse("target ESP GUID is malformed or names a staging partition");
    }
    let removed = delete_owned_entry(host, &handoff, out)?;
    let mut candidates = target_entries(host, &target_esp)?;
    if candidates.is_empty()
        && !request.target_disk.is_empty()
        && request.target_esp_partition_number != 0
        && !request.loader.is_empty()
    {
        let order = boot_ids(host, "BootOrder")?;
        let number = request.target_esp_partition_number.to_string();
        efibootmgr_run(
            host,
            &[
                "-c",
                "-d",
                &request.target_disk,
                "-p",
                &number,
                "-L",
                "Tilo",
                "-l",
                &request.loader,
            ],
        )?;
        if boot_ids(host, "BootOrder")? != order {
            if order.is_empty() {
                efibootmgr_run(host, &["-O"])?;
            } else {
                efibootmgr_run(host, &["-o", &order.join(",")])?;
            }
        }
        if boot_ids(host, "BootOrder")? != order {
            return refuse("BootOrder changed while creating the Tilo boot entry");
        }
        candidates = target_entries(host, &target_esp)?;
    }
    let [candidate] = candidates.as_slice() else {
        return refuse(format!(
            "expected exactly one firmware entry for the installed Tilo ESP, found {}",
            candidates.len()
        ));
    };
    efibootmgr_run(host, &["-n", candidate])?;
    if boot_ids(host, "BootNext")? != [candidate.clone()] {
        return refuse("BootNext did not persist");
    }
    let _ = writeln!(
        out,
        "the next restart starts the installed Tilo (Boot{candidate}) once; the default is unchanged"
    );
    Ok(if removed { 0 } else { 3 })
}

// --- entry point -------------------------------------------------------------

const PROG: &str = "tilo-staging-handoff";
const COMMANDS: [&str; 9] = [
    "detect",
    "check-install",
    "check-mounts",
    "write-reclaim-record",
    "firmware-finalize",
    "preflight-retained",
    "deliver-retained",
    "required-user-id",
    "deliver-user-plan",
];
const USAGE: &str = "usage: tilo-staging-handoff [-h]
                            {detect,check-install,check-mounts,write-reclaim-record,firmware-finalize,preflight-retained,deliver-retained,required-user-id,deliver-user-plan,check-device-label,write-machine-info,check-prepared-choices,guided-geometry,gpt,unpack-setup-handoff}
                            ...
";
const HELP: &str = "
positional arguments:
  {detect,check-install,check-mounts,write-reclaim-record,firmware-finalize,preflight-retained,deliver-retained,required-user-id,deliver-user-plan,check-device-label,write-machine-info,check-prepared-choices,guided-geometry,gpt,unpack-setup-handoff}

options:
  -h, --help            show this help message and exit
";

const fn option(flag: &'static str, required: bool) -> Arg {
    Arg::Option { flag, required }
}

const DETECT: Command = Command {
    prog: "tilo-staging-handoff detect",
    usage: "usage: tilo-staging-handoff detect [-h] [--out OUT] [--error ERROR]\n",
    help: "\noptions:\n  -h, --help     show this help message and exit\n  --out OUT\n  --error ERROR\n",
    args: &[option("--out", false), option("--error", false)],
};

const PLAN_FLAGS: [&str; 9] = [
    "--free-start-bytes",
    "--free-size-bytes",
    "--efi-start-bytes",
    "--efi-size-bytes",
    "--boot-start-bytes",
    "--boot-size-bytes",
    "--root-start-bytes",
    "--root-size-bytes",
    "--sector-size",
];

const CHECK_INSTALL: Command = Command {
    prog: "tilo-staging-handoff check-install",
    usage: "usage: tilo-staging-handoff check-install [-h] --handoff HANDOFF --disk DISK
                                          --mode {guided,erase,prepared}
                                          [--free-start-bytes FREE_START_BYTES]
                                          [--free-size-bytes FREE_SIZE_BYTES]
                                          [--efi-start-bytes EFI_START_BYTES]
                                          [--efi-size-bytes EFI_SIZE_BYTES]
                                          [--boot-start-bytes BOOT_START_BYTES]
                                          [--boot-size-bytes BOOT_SIZE_BYTES]
                                          [--root-start-bytes ROOT_START_BYTES]
                                          [--root-size-bytes ROOT_SIZE_BYTES]
                                          [--sector-size SECTOR_SIZE]
",
    help: "\noptions:\n  -h, --help            show this help message and exit\n",
    args: &[
        option("--handoff", true),
        option("--disk", true),
        option("--mode", true),
        option("--free-start-bytes", false),
        option("--free-size-bytes", false),
        option("--efi-start-bytes", false),
        option("--efi-size-bytes", false),
        option("--boot-start-bytes", false),
        option("--boot-size-bytes", false),
        option("--root-start-bytes", false),
        option("--root-size-bytes", false),
        option("--sector-size", false),
    ],
};

const CHECK_MOUNTS: Command = Command {
    prog: "tilo-staging-handoff check-mounts",
    usage: "usage: tilo-staging-handoff check-mounts [-h] --handoff HANDOFF --disk DISK\n",
    help: "\noptions:\n  -h, --help         show this help message and exit\n  --handoff HANDOFF\n  --disk DISK\n",
    args: &[option("--handoff", true), option("--disk", true)],
};

const RECORD: Command = Command {
    prog: "tilo-staging-handoff write-reclaim-record",
    usage: "usage: tilo-staging-handoff write-reclaim-record [-h] --handoff HANDOFF
                                                 --target-var TARGET_VAR
                                                 --target-root-partuuid
                                                 TARGET_ROOT_PARTUUID
                                                 --target-esp-partuuid
                                                 TARGET_ESP_PARTUUID
                                                 --target-root-start-bytes
                                                 TARGET_ROOT_START_BYTES
                                                 --target-root-size-bytes
                                                 TARGET_ROOT_SIZE_BYTES
                                                 --sector-size SECTOR_SIZE
",
    help: "\noptions:\n  -h, --help            show this help message and exit\n",
    args: &[
        option("--handoff", true),
        option("--target-var", true),
        option("--target-root-partuuid", true),
        option("--target-esp-partuuid", true),
        option("--target-root-start-bytes", true),
        option("--target-root-size-bytes", true),
        option("--sector-size", true),
    ],
};

const FINALIZE: Command = Command {
    prog: "tilo-staging-handoff firmware-finalize",
    usage: "usage: tilo-staging-handoff firmware-finalize [-h] --handoff HANDOFF
                                              --target-esp-partuuid
                                              TARGET_ESP_PARTUUID
                                              [--target-disk TARGET_DISK]
                                              [--target-esp-partition-number TARGET_ESP_PARTITION_NUMBER]
                                              [--loader LOADER]
",
    help: "\noptions:\n  -h, --help            show this help message and exit\n",
    args: &[
        option("--handoff", true),
        option("--target-esp-partuuid", true),
        option("--target-disk", false),
        option("--target-esp-partition-number", false),
        option("--loader", false),
    ],
};

/// What a run of the command line printed, and its exit status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    fn usage(command: &Command, message: &str) -> Self {
        Self {
            code: 2,
            stdout: String::new(),
            stderr: format!("{}{}: error: {message}\n", command.usage, command.prog),
        }
    }
}

fn int_value(
    command: &Command,
    values: &args::Values,
    flag: &str,
    default: i128,
) -> std::result::Result<i128, Outcome> {
    match values.get(flag) {
        None => Ok(default),
        Some(raw) => {
            let text = raw.to_string_lossy();
            crate::py::int(&text).map_err(|_| {
                Outcome::usage(
                    command,
                    &format!("argument {flag}: invalid int value: '{text}'"),
                )
            })
        }
    }
}

fn text_value(values: &args::Values, flag: &str, default: &str) -> String {
    values.get(flag).map_or_else(
        || default.to_owned(),
        |value| value.to_string_lossy().into_owned(),
    )
}

fn parse_sub(command: &Command, rest: &[OsString]) -> std::result::Result<args::Values, Outcome> {
    match args::parse(command, rest) {
        Ok(args::Parsed::Run(values)) => Ok(values),
        Ok(args::Parsed::Help(text)) => Err(Outcome {
            code: 0,
            stdout: text,
            stderr: String::new(),
        }),
        Err(stderr) => Err(Outcome {
            code: 2,
            stdout: String::new(),
            stderr,
        }),
    }
}

/// A parsed subcommand's run: its exit status and what it printed, or a
/// usage outcome (help or error) already decided by the parser.
type SubOutcome = std::result::Result<Outcome, Outcome>;

/// The outcome of a subcommand's `result`, with what it printed.
fn finish(result: Result<u8>, stdout: String, stderr: String) -> Outcome {
    match result {
        Ok(code) => Outcome {
            code,
            stdout,
            stderr,
        },
        Err(fail) => Outcome {
            code: 1,
            stdout,
            stderr: format!("{stderr}error: staged installation: {fail}\n"),
        },
    }
}

fn run_detect(host: &dyn Host, rest: &[OsString]) -> SubOutcome {
    let values = parse_sub(&DETECT, rest)?;
    let out = text_value(&values, "--out", "/run/tilo-installer/handoff.json");
    let error = text_value(&values, "--error", "/run/tilo-installer/handoff.error");
    Ok(match detect(host, Path::new(&out), Path::new(&error)) {
        Ok(printed) => finish(Ok(0), printed.stdout, printed.stderr),
        Err(fail) => finish(Err(fail), String::new(), String::new()),
    })
}

fn run_check_install(host: &dyn Host, rest: &[OsString]) -> SubOutcome {
    let values = parse_sub(&CHECK_INSTALL, rest)?;
    let mode = text_value(&values, "--mode", "");
    if !["guided", "erase", "prepared"].contains(&mode.as_str()) {
        return Err(Outcome::usage(
            &CHECK_INSTALL,
            &format!(
                "argument --mode: invalid choice: '{mode}' (choose from guided, erase, prepared)"
            ),
        ));
    }
    let mut numbers = [0i128; 9];
    for (slot, flag) in numbers.iter_mut().zip(PLAN_FLAGS) {
        *slot = int_value(&CHECK_INSTALL, &values, flag, 0)?;
    }
    let [
        free_start_bytes,
        free_size_bytes,
        efi_start_bytes,
        efi_size_bytes,
        boot_start_bytes,
        boot_size_bytes,
        root_start_bytes,
        root_size_bytes,
        sector_size,
    ] = numbers;
    let plan = Plan {
        free_start_bytes,
        free_size_bytes,
        efi_start_bytes,
        efi_size_bytes,
        boot_start_bytes,
        boot_size_bytes,
        root_start_bytes,
        root_size_bytes,
        sector_size,
    };
    let checked = check_install(
        host,
        Path::new(&text_value(&values, "--handoff", "")),
        &text_value(&values, "--disk", ""),
        &mode,
        &plan,
    );
    Ok(match checked {
        Ok(facts) => finish(Ok(0), facts, String::new()),
        Err(fail) => finish(Err(fail), String::new(), String::new()),
    })
}

fn run_check_mounts(host: &dyn Host, rest: &[OsString]) -> SubOutcome {
    let values = parse_sub(&CHECK_MOUNTS, rest)?;
    let checked = check_mounts_command(
        host,
        Path::new(&text_value(&values, "--handoff", "")),
        &text_value(&values, "--disk", ""),
    );
    Ok(finish(checked.map(|()| 0), String::new(), String::new()))
}

fn run_record(rest: &[OsString]) -> SubOutcome {
    let values = parse_sub(&RECORD, rest)?;
    let target = TargetRoot {
        root_partuuid: text_value(&values, "--target-root-partuuid", ""),
        esp_partuuid: text_value(&values, "--target-esp-partuuid", ""),
        root_start_bytes: int_value(&RECORD, &values, "--target-root-start-bytes", 0)?,
        root_size_bytes: int_value(&RECORD, &values, "--target-root-size-bytes", 0)?,
        sector_size: int_value(&RECORD, &values, "--sector-size", 0)?,
    };
    let recorded = write_reclaim_record(
        Path::new(&text_value(&values, "--handoff", "")),
        Path::new(&text_value(&values, "--target-var", "")),
        &target,
    );
    Ok(match recorded {
        Ok(printed) => finish(Ok(0), printed, String::new()),
        Err(fail) => finish(Err(fail), String::new(), String::new()),
    })
}

fn run_finalize(host: &dyn Host, rest: &[OsString]) -> SubOutcome {
    let values = parse_sub(&FINALIZE, rest)?;
    let request = Finalize {
        target_esp_partuuid: text_value(&values, "--target-esp-partuuid", ""),
        target_disk: text_value(&values, "--target-disk", ""),
        target_esp_partition_number: int_value(
            &FINALIZE,
            &values,
            "--target-esp-partition-number",
            0,
        )?,
        loader: text_value(&values, "--loader", ""),
    };
    let mut stdout = String::new();
    let result = firmware_finalize(
        host,
        Path::new(&text_value(&values, "--handoff", "")),
        &request,
        &mut stdout,
    );
    Ok(finish(result, stdout, String::new()))
}

fn dispatch(host: &dyn Host, command: &str, rest: &[OsString]) -> SubOutcome {
    match command {
        "preflight-retained" | "deliver-retained" => Ok(finish(
            retained_commands::run(host, command, rest, validate_migration).map(|()| 0),
            String::new(),
            String::new(),
        )),
        "required-user-id" | "deliver-user-plan" => {
            Ok(match user_commands::run(host, command, rest) {
                Ok(stdout) => finish(Ok(0), stdout, String::new()),
                Err(error) => finish(Err(error), String::new(), String::new()),
            })
        }
        "detect" => run_detect(host, rest),
        "check-install" => run_check_install(host, rest),
        "check-mounts" => run_check_mounts(host, rest),
        "write-reclaim-record" => run_record(rest),
        _ => run_finalize(host, rest),
    }
}

/// Runs the command line (without the program name).
pub fn run(host: &dyn Host, args: &[OsString]) -> Outcome {
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let Some(first) = words.first() else {
        return Outcome {
            code: 2,
            stdout: String::new(),
            stderr: USAGE.to_owned(),
        };
    };
    if first == "-h" || (first.len() > 2 && "--help".starts_with(first.as_str())) {
        return Outcome {
            code: 0,
            stdout: format!("{USAGE}{HELP}"),
            stderr: String::new(),
        };
    }
    if backend_commands::COMMANDS.contains(&first.as_str()) {
        return backend_commands::run(first, args.get(1..).unwrap_or_default());
    }
    if !COMMANDS.contains(&first.as_str()) {
        let message = if first.starts_with('-') {
            format!("unrecognized arguments: {first}")
        } else {
            format!(
                "argument command: invalid choice: '{first}' (choose from {})",
                COMMANDS.join(", ")
            )
        };
        return Outcome {
            code: 2,
            stdout: String::new(),
            stderr: format!("{USAGE}{PROG}: error: {message}\n"),
        };
    }
    match dispatch(host, first, args.get(1..).unwrap_or_default()) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

/// Runs the command line and prints what it printed.
pub fn main(host: &dyn Host, args: &[OsString]) -> ExitCode {
    let outcome = run(host, args);
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    ExitCode::from(outcome.code)
}

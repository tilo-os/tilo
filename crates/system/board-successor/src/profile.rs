//! The installed hardware profile, read exactly as
//! `os/dev/hardware_profile.py`, the image builders' module, reads
//! it: `read_profile`, `development_origin`, `validate_target_imgref`,
//! `trusted_path` and `resolve_profile` with what they call.
//!
//! The installer resolves its update origin here too
//! (`tilo-board-successor install-update-origin`); the module stays for the
//! image builders. A change to what it
//! accepts must be made in both; `tests/profile.rs` holds both to the same
//! fixtures.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::py;
use crate::{Error, Result, require};

pub const PROFILE_PATH: &str = "usr/lib/tilo/hardware-profile.json";
pub const DEFINITION_PATH: &str = "usr/lib/tilo/hardware-profile-definition.json";
const DEVELOPMENT_PATH: &str = "usr/share/tilo/development-trust";
const ARCHITECTURES: [&str; 2] = ["x86_64", "aarch64"];
const FIELDS: [&str; 5] = [
    "schema_version",
    "id",
    "architecture",
    "devicetree",
    "firmware",
];
const OPTIONAL_V2: [&str; 8] = [
    "boot_modules",
    "always_on",
    "kernel_args",
    "wifi_wake_on_wlan",
    "internal_keyboards",
    "firmware_exclude",
    "crash_record",
    "dmi",
];
/// The DMI system identity of a board whose firmware describes its devices,
/// which has no device tree for a model-bound quirk to match on.
const DMI_FIELDS: [&str; 2] = ["sys_vendor", "product_name"];
/// Where the kernel keeps a record of a panic, what resets a hung machine,
/// and which detector panics first. Every definition declares all three.
const CRASH_RECORD_FIELDS: [&str; 3] = ["storage", "watchdog", "lockup"];
const CRASH_BACKENDS: [&str; 4] = ["ramoops", "efi-variable", "block", "none"];
const CRASH_WATCHDOGS: [&str; 3] = ["hardware", "module", "none"];
const CRASH_LOCKUP: [&str; 3] = ["buddy", "nmi", "none"];
/// Kernel arguments that make the watchdog reset a machine nobody feeds.
const CRASH_WATCHDOG_ARGS: [&str; 3] =
    ["systemd.watchdog_sec=", "sbsa_gwdt.action=1", "nowayout=1"];
/// The argument that makes a stuck CPU panic rather than only log, per detector.
const CRASH_LOCKUP_ARGS: [(&str, &[&str]); 2] = [
    (
        "buddy",
        &[
            "nmi_watchdog=panic",
            "nmi_watchdog=1",
            "softlockup_panic=1",
            "hardlockup_panic=1",
        ],
    ),
    (
        "nmi",
        &[
            "nmi_watchdog=panic",
            "softlockup_panic=1",
            "hardlockup_panic=1",
        ],
    ),
];
/// Every argument any detector panics from, so a board that declares its stuck
/// CPU only logs is refused when its own command line makes one panic.
const CRASH_PANIC_ARGS: [&str; 4] = [
    "nmi_watchdog=panic",
    "nmi_watchdog=1",
    "softlockup_panic=1",
    "hardlockup_panic=1",
];
const PROFILE_FIELDS: [&str; 6] = [
    "schema_version",
    "id",
    "architecture",
    "devicetree",
    "devicetree_sha256",
    "target_update_ref",
];
const INTERNAL_KEYBOARD_FIELDS: [&str; 5] =
    ["name", "bus", "vendor", "product", "devicetree_compatible"];
/// Fedora firmware RPMs a board layer may remove, each with the subtrees of
/// /usr/lib/firmware it owns.
const FIRMWARE_PACKAGE_TREES: [(&str, &[&str]); 10] = [
    ("nvidia-gpu-firmware", &["nvidia"]),
    ("amd-gpu-firmware", &["amdgpu", "radeon"]),
    ("intel-gpu-firmware", &["i915", "xe"]),
    (
        "intel-audio-firmware",
        &["intel/avs", "intel/sof", "intel/sof-ipc4", "intel/sof-tplg"],
    ),
    ("intel-npu-firmware", &["intel/vpu"]),
    ("amd-ucode-firmware", &["amd-ucode"]),
    ("mt7xxx-firmware", &["mediatek"]),
    ("brcmfmac-firmware", &["brcm", "cypress"]),
    ("nxpwireless-firmware", &["nxp"]),
    ("tiwilink-firmware", &["ti-connectivity"]),
];

fn full(pattern: &str) -> Regex {
    // Patterns are constants checked by the unit tests; an invalid one is a
    // build defect, so fall back to a regex that matches nothing.
    Regex::new(&format!("^(?:{pattern})$")).unwrap_or_else(|_| never())
}

fn never() -> Regex {
    Regex::new("[^\\s\\S]").unwrap_or_else(|_| never())
}

macro_rules! pattern {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| full($pattern));
    };
}

pattern!(
    TILO_VM_REF,
    r"registry\.tilo\.so/tilo/tilo-vm(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})"
);
pattern!(
    RELEASE_REF,
    r"(?:registry\.tilo\.so|code\.tilo\.so)/tilo/tilo(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})"
);
pattern!(
    DEVELOPMENT_REF,
    r"([a-z0-9](?:[a-z0-9.-]*[a-z0-9])?)(?::([0-9]{1,5}))?/(tilo-development/os|tilo/tilo-development)(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})"
);
pattern!(
    TAG_ARCHITECTURE,
    r"(?:[A-Za-z0-9_][A-Za-z0-9_.-]*-)?(x86_64|aarch64)"
);
pattern!(OWNER_KEY, r"[0-9a-f]{64}");
pattern!(PROFILE_ID, r"[a-z0-9][a-z0-9-]{0,63}");
pattern!(PATH_PART, r"[A-Za-z0-9_.+,-]+");
pattern!(MODULE_NAME, r"[a-z0-9][a-z0-9_-]{0,63}");
pattern!(KEYBOARD_ID, r"[0-9A-F]{4}");
pattern!(KEYBOARD_NAME, r"[A-Za-z0-9][A-Za-z0-9 :._()-]{0,79}");
pattern!(COMPATIBLE, r"[a-z0-9][a-z0-9._-]*,[a-z0-9][a-z0-9._-]*");
pattern!(DMI_VALUE, r"[A-Za-z0-9][A-Za-z0-9 ._()-]{0,63}");
pattern!(KERNEL_VERSION, r"[A-Za-z0-9][A-Za-z0-9._+-]*");
pattern!(DTB_DIGEST, r"[0-9a-f]{64}");
pattern!(
    KERNEL_ARG,
    r"[A-Za-z0-9_.-]{1,64}(?:=[A-Za-z0-9_.,:/-]{1,128})*"
);

/// The image root the profile is read from, and who must own its inputs
/// (root in the image; the tests' own user in a fixture).
#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub owner: u32,
}

impl Root {
    /// `/`, owned by root.
    #[must_use]
    pub fn system() -> Self {
        Self {
            path: PathBuf::from("/"),
            owner: 0,
        }
    }

    fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// `trusted_path`: every component from the root down must be a real
    /// directory (the last a regular file when `regular`), owned by the
    /// owner and not group- or world-writable.
    ///
    /// # Errors
    /// A symlink, a foreign owner, a writable component, the wrong type, or
    /// a component that cannot be examined.
    pub fn trusted_path(&self, relative: &str, regular: bool) -> Result<PathBuf> {
        let parts: Vec<&str> = relative
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        let mut current = self.path.clone();
        for index in 0..=parts.len() {
            if let Some(part) = index.checked_sub(1).and_then(|i| parts.get(i)) {
                current.push(part);
            }
            let info = fs::symlink_metadata(&current).map_err(|e| Error::os(&e, &current))?;
            let shown = current.display();
            require(
                !info.file_type().is_symlink(),
                format!("hardware profile input contains a symlink: {shown}"),
            )?;
            require(
                info.uid() == self.owner && info.mode() & 0o022 == 0,
                format!(
                    "hardware profile input must be root-owned and not group/world writable: {shown}"
                ),
            )?;
            if regular && index == parts.len() {
                require(
                    info.file_type().is_file(),
                    format!("hardware profile input is not a regular file: {shown}"),
                )?;
            } else {
                require(
                    info.file_type().is_dir(),
                    format!("hardware profile input is not a directory: {shown}"),
                )?;
            }
        }
        Ok(current)
    }

    /// `Path.exists() or Path.is_symlink()`.
    fn present(&self, relative: &str) -> bool {
        let path = self.join(relative);
        fs::metadata(&path).is_ok()
            || fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
    }

    /// A trusted regular file of at most 64 KiB (`development_origin`'s `read`).
    fn read_bounded(&self, relative: &str) -> Result<Vec<u8>> {
        let path = self.trusted_path(relative, true)?;
        let size = fs::metadata(&path).map_err(|e| Error::os(&e, &path))?.len();
        require(size <= 65536, "development origin input is too large")?;
        fs::read(&path).map_err(|e| Error::os(&e, &path))
    }
}

pub(crate) fn json_bytes(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|e| Error::Value(e.to_string()))
}

fn json_text(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::Value(e.to_string()))?;
    serde_json::from_str(text).map_err(|e| Error::Value(e.to_string()))
}

/// `value[key]` on a dict, `KeyError`/`TypeError` otherwise.
pub(crate) fn index<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    match value {
        Value::Object(map) => map.get(key).ok_or_else(|| Error::Value(format!("'{key}'"))),
        _ => Err(Error::Value(format!(
            "cannot index a non-object with '{key}'"
        ))),
    }
}

/// `value.get(key, default)` on a dict; `AttributeError` otherwise.
pub(crate) fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        _ => Err(Error::Value(format!(
            "'{}' object has no attribute 'get'",
            type_name(value)
        ))),
    }
}

pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn keys(map: &Map<String, Value>) -> BTreeSet<&str> {
    map.keys().map(String::as_str).collect()
}

fn names<'a>(list: &[&'a str]) -> BTreeSet<&'a str> {
    list.iter().copied().collect()
}

/// The profile's fields, or `None` when the image has no hardware profile.
///
/// # Errors
/// A profile, definition or update origin the module refuses.
pub fn read_profile(root: &Root) -> Result<Option<Map<String, Value>>> {
    if !root.present(PROFILE_PATH) {
        require(
            !root.present(DEFINITION_PATH),
            "hardware definition has no generated hardware profile",
        )?;
        return Ok(None);
    }
    let profile = root.trusted_path(PROFILE_PATH, true)?;
    let size = fs::metadata(&profile)
        .map_err(|e| Error::os(&e, &profile))?
        .len();
    require(size <= 16384, "hardware profile is too large")?;
    let data = json_text(&fs::read(&profile).map_err(|e| Error::os(&e, &profile))?)?;
    let Value::Object(data) = data else {
        return Err(Error::value(
            "hardware profile has missing or unknown fields",
        ));
    };
    require(
        keys(&data) == names(&PROFILE_FIELDS),
        "hardware profile has missing or unknown fields",
    )?;
    let schema = data.get("schema_version").and_then(py::as_int);
    require(schema == Some(1), "unsupported hardware profile schema")?;
    let definition = read_definition(&root.trusted_path(DEFINITION_PATH, true)?)?;
    require(
        data.get("id") == definition.get("id"),
        "hardware profile id does not match its image-owned definition",
    )?;
    require(
        data.get("architecture") == definition.get("architecture"),
        "hardware profile requires a matching native image architecture",
    )?;
    let target = data.get("target_update_ref").unwrap_or(&Value::Null);
    validate_target_imgref(target, root)?;
    if development_origin(root)?.is_some() {
        let architecture = data.get("architecture").unwrap_or(&Value::Null);
        development_target(target, Some(architecture))?;
    }
    Ok(Some(data))
}

/// `read_definition`: a regular file of at most 64 KiB, then
/// [`validate_definition`].
///
/// # Errors
/// See [`validate_definition`].
pub fn read_definition(path: &Path) -> Result<Map<String, Value>> {
    let link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let metadata = fs::metadata(path).ok();
    require(
        !link
            && metadata.as_ref().is_some_and(fs::Metadata::is_file)
            && metadata.is_some_and(|m| m.len() <= 65536),
        "hardware definition must be a regular bounded file",
    )?;
    validate_definition(json_text(
        &fs::read(path).map_err(|e| Error::os(&e, path))?,
    )?)
}

fn as_str(value: &Value) -> Option<&str> {
    value.as_str()
}

/// `relative_path`: `/`-separated parts, none empty, `.` or `..`, each of
/// `[A-Za-z0-9_.+,-]`. Returns the number of parts.
fn relative_path(value: &Value) -> Result<usize> {
    let error = || Error::value("hardware definition path must be a safe relative path");
    let text = as_str(value).filter(|t| !t.is_empty()).ok_or_else(error)?;
    let parts: Vec<&str> = text.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." || !PATH_PART.is_match(part) {
            return Err(error());
        }
    }
    Ok(parts.len())
}

/// `PurePath(text).suffix`.
fn suffix(text: &str) -> &str {
    let name = text.rsplit('/').next().unwrap_or(text);
    match name.rfind('.') {
        Some(at) if at > 0 && at.saturating_add(1) < name.len() => {
            name.get(at..).unwrap_or_default()
        }
        _ => "",
    }
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

fn optional_list<'a>(
    data: &'a Map<String, Value>,
    key: &str,
    limit: usize,
    message: &str,
) -> Result<&'a [Value]> {
    match data.get(key) {
        None => Ok(&[]),
        Some(Value::Array(items)) if items.len() <= limit => Ok(items),
        Some(_) => Err(Error::value(message)),
    }
}

/// `validate_definition`.
///
/// # Errors
/// Every refusal of the module's own validation.
pub fn validate_definition(data: Value) -> Result<Map<String, Value>> {
    let Value::Object(data) = data else {
        return Err(Error::value(
            "hardware definition has missing or unknown fields",
        ));
    };
    let schema = data.get("schema_version");
    let schema_number = schema.and_then(py::as_int);
    let (expected, optional): (BTreeSet<&str>, BTreeSet<&str>) = match schema_number {
        Some(1) => (names(&FIELDS), BTreeSet::new()),
        Some(2) => {
            let mut expected = names(&FIELDS);
            expected.insert("system_firmware");
            (expected, names(&OPTIONAL_V2))
        }
        _ => return Err(Error::value("unsupported hardware definition schema")),
    };
    let present = keys(&data);
    let allowed: BTreeSet<&str> = expected.union(&optional).copied().collect();
    require(
        present.is_subset(&allowed) && expected.is_subset(&present),
        "hardware definition has missing or unknown fields",
    )?;
    let field = |key: &str| data.get(key).unwrap_or(&Value::Null);
    let id = as_str(field("id")).filter(|id| PROFILE_ID.is_match(id));
    let id = id.ok_or_else(|| Error::value("invalid hardware profile id"))?;
    require(
        id != "generic",
        "generic is reserved for the standard firmware boot path",
    )?;
    let architecture = as_str(field("architecture")).filter(|a| ARCHITECTURES.contains(a));
    let architecture =
        architecture.ok_or_else(|| Error::value("unsupported hardware definition architecture"))?;
    let devicetree = field("devicetree");
    if !devicetree.is_null() {
        relative_path(devicetree)?;
        require(
            architecture == "aarch64" && suffix(as_str(devicetree).unwrap_or_default()) == ".dtb",
            "device-tree profiles require an aarch64 DTB",
        )?;
    }
    validate_firmware(&data)?;
    let boot_modules = optional_list(
        &data,
        "boot_modules",
        64,
        "hardware definition boot modules must be a bounded name list",
    )?;
    let mut seen_modules = BTreeSet::new();
    for name in boot_modules {
        let name = as_str(name)
            .filter(|n| MODULE_NAME.is_match(n))
            .ok_or_else(|| Error::value("hardware definition boot module name is invalid"))?;
        require(
            seen_modules.insert(name),
            "hardware definition boot module names must be unique",
        )?;
    }
    require(
        data.get("always_on").is_none_or(Value::is_boolean),
        "hardware definition always_on must be a boolean",
    )?;
    if let Some(wake) = data.get("wifi_wake_on_wlan") {
        require(
            py::as_int(wake) == Some(0)
                && id == "lenovo-83ed"
                && architecture == "aarch64"
                && devicetree.as_str() == Some("qcom/x1e80100-lenovo-yoga-slim7x.dtb"),
            "Wi-Fi wake policy requires the Lenovo 83ED definition and disabled WoWLAN",
        )?;
    }
    validate_dmi(&data)?;
    validate_internal_keyboards(&data)?;
    let kernel_args = optional_list(
        &data,
        "kernel_args",
        16,
        "hardware definition kernel arguments must be a bounded list",
    )?;
    let mut seen_args = BTreeSet::new();
    for argument in kernel_args {
        let argument = as_str(argument)
            .filter(|a| KERNEL_ARG.is_match(a))
            .ok_or_else(|| Error::value("hardware definition kernel argument is invalid"))?;
        seen_args.insert(argument);
    }
    require(
        seen_args.len() == kernel_args.len(),
        "hardware definition kernel arguments must be unique",
    )?;
    validate_crash_record(&data, &seen_args, schema_number == Some(2))?;
    Ok(data)
}

/// `validate_crash_record`: the board says where its crash record goes, what
/// resets it and what panics first, and its kernel arguments agree.
fn validate_crash_record(
    data: &Map<String, Value>,
    kernel_args: &BTreeSet<&str>,
    required: bool,
) -> Result<()> {
    if !required {
        // Schema 1 predates the record and is still accepted as-is; a board
        // that moves to schema 2 states it.
        return Ok(());
    }
    let record = match data.get("crash_record") {
        Some(Value::Object(record)) if record.len() <= 16 => record,
        _ => {
            return Err(Error::value(
                "hardware definition crash record must name a storage backend, a watchdog and a lockup detector",
            ));
        }
    };
    let present = keys(record);
    require(
        present.iter().all(|key| CRASH_RECORD_FIELDS.contains(key))
            && CRASH_RECORD_FIELDS.iter().all(|key| present.contains(key)),
        "hardware definition crash record has missing or unknown fields",
    )?;
    let text = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or_default();
    let storage = text("storage");
    let watchdog = text("watchdog");
    let lockup = text("lockup");
    require(
        CRASH_BACKENDS.contains(&storage),
        "hardware definition crash record storage must name a supported backend",
    )?;
    require(
        CRASH_WATCHDOGS.contains(&watchdog),
        "hardware definition crash record watchdog must name a supported watchdog",
    )?;
    require(
        CRASH_LOCKUP.contains(&lockup),
        "hardware definition crash record lockup must name a supported detector",
    )?;
    // ramoops and pstore/blk are memory the board's own device tree reserves.
    require(
        !(matches!(storage, "ramoops" | "block")
            && data.get("devicetree").unwrap_or(&Value::Null).is_null()),
        "hardware definition crash record storage requires a device-tree board",
    )?;
    let feeds_watchdog = kernel_args.iter().any(|arg| {
        CRASH_WATCHDOG_ARGS
            .iter()
            .any(|prefix| arg.starts_with(prefix))
    });
    require(
        (watchdog != "none") == feeds_watchdog,
        "hardware definition crash record watchdog must name the watchdog its arguments feed",
    )?;
    // A board whose detector only reports must not panic from a stuck CPU.
    let panic_args: &[&str] = if lockup == "none" {
        CRASH_PANIC_ARGS.as_slice()
    } else {
        CRASH_LOCKUP_ARGS
            .iter()
            .find(|(name, _)| *name == lockup)
            .map_or(&[], |(_, args)| args)
    };
    let panics = panic_args.iter().any(|arg| kernel_args.contains(arg));
    require(
        (lockup != "none") == panics,
        "hardware definition crash record lockup must name the detector its arguments panic from",
    )?;
    // The watchdog driver has to reach the initramfs to be fed from boot.
    let modular_watchdog = optional_list(
        data,
        "boot_modules",
        64,
        "hardware definition boot modules must be a bounded name list",
    )?
    .iter()
    .filter_map(Value::as_str)
    .any(|name| name.ends_with("_wdt"));
    require(
        watchdog != "module" || modular_watchdog,
        "hardware definition crash record watchdog module must be a boot module",
    )
}

/// The firmware mapping, system firmware and removable firmware packages of
/// a definition: bounded, unique, non-overlapping, and never removing what
/// the definition names.
fn validate_firmware(data: &Map<String, Value>) -> Result<()> {
    let field = |key: &str| data.get(key).unwrap_or(&Value::Null);
    let firmware = match field("firmware") {
        Value::Object(map) if map.len() <= 256 => map,
        _ => {
            return Err(Error::value(
                "hardware definition firmware must be a bounded file mapping",
            ));
        }
    };
    let mut destinations: Vec<String> = Vec::new();
    for (source, destination) in firmware {
        require(
            relative_path(&Value::String(source.clone()))? == 1,
            "hardware firmware source must be a basename",
        )?;
        relative_path(destination)?;
        let destination = as_str(destination).unwrap_or_default().to_owned();
        require(
            !destinations
                .iter()
                .any(|other| overlaps(&destination, other)),
            "hardware firmware destinations must be unique and must not overlap",
        )?;
        destinations.push(destination);
    }
    let system_firmware = optional_list(
        data,
        "system_firmware",
        256,
        "hardware definition system firmware must be a bounded path list",
    )?;
    let mut system_paths: Vec<String> = Vec::new();
    for path in system_firmware {
        relative_path(path)?;
        let path = as_str(path).unwrap_or_default().to_owned();
        require(
            !system_paths.iter().any(|other| overlaps(&path, other)),
            "hardware definition system firmware paths must be unique and must not overlap",
        )?;
        require(
            !destinations
                .iter()
                .any(|destination| overlaps(&path, destination)),
            "system firmware must not overlap local firmware destinations",
        )?;
        system_paths.push(path);
    }
    let excluded = optional_list(
        data,
        "firmware_exclude",
        FIRMWARE_PACKAGE_TREES.len(),
        "hardware definition firmware_exclude must be a bounded package list",
    )?;
    let mut seen_packages = BTreeSet::new();
    for package in excluded {
        // `str()` of a non-string never names a removable package; Python
        // refused it here or one check later.
        let shown = as_str(package).map_or_else(|| format!("\u{0}{package}"), str::to_owned);
        require(
            seen_packages.insert(shown),
            "hardware definition firmware_exclude packages must be unique",
        )?;
    }
    let needed: Vec<&String> = system_paths.iter().chain(destinations.iter()).collect();
    for package in excluded {
        let trees = as_str(package)
            .and_then(|name| {
                FIRMWARE_PACKAGE_TREES
                    .iter()
                    .find(|(known, _)| *known == name)
            })
            .map(|(_, trees)| *trees)
            .ok_or_else(|| {
                Error::value(
                    "hardware definition firmware_exclude names a package that may not be removed",
                )
            })?;
        for tree in trees {
            require(
                !needed.iter().any(|path| overlaps(path, tree)),
                format!(
                    "firmware_exclude {} would remove firmware the definition names",
                    as_str(package).unwrap_or_default()
                ),
            )?;
        }
    }
    Ok(())
}

fn validate_internal_keyboards(data: &Map<String, Value>) -> Result<()> {
    let keyboards = optional_list(
        data,
        "internal_keyboards",
        8,
        "hardware definition internal keyboards must be a bounded list",
    )?;
    require(
        keyboards.is_empty()
            || !data.get("devicetree").unwrap_or(&Value::Null).is_null()
            || data.get("dmi").is_some(),
        "hardware definition internal keyboards require a device-tree or DMI board",
    )?;
    let mut seen = BTreeSet::new();
    for keyboard in keyboards {
        let keyboard = match keyboard {
            Value::Object(map) if keys(map) == names(&INTERNAL_KEYBOARD_FIELDS) => map,
            _ => {
                return Err(Error::value(
                    "hardware definition internal keyboard has missing or unknown fields",
                ));
            }
        };
        require(
            keyboard
                .values()
                .all(|value| value.is_string() || value.is_null()),
            "hardware definition internal keyboard fields must be strings",
        )?;
        let text = |key: &str| {
            keyboard
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        require(
            matches!(text("bus"), "i2c" | "spi"),
            "hardware definition internal keyboard bus must be i2c or spi",
        )?;
        require(
            KEYBOARD_ID.is_match(text("vendor")) && KEYBOARD_ID.is_match(text("product")),
            "hardware definition internal keyboard ids must be four uppercase hex digits",
        )?;
        require(
            KEYBOARD_NAME.is_match(text("name")),
            "hardware definition internal keyboard name is invalid",
        )?;
        // A device-tree board confines the quirk by its compatible; a
        // firmware-described one has no compatible and is confined by the
        // definition's DMI identity instead. Exactly one applies, so a quirk
        // can never name an identity that cannot both apply and be true.
        let board_has_devicetree = !data.get("devicetree").unwrap_or(&Value::Null).is_null();
        require(
            if board_has_devicetree {
                COMPATIBLE.is_match(text("devicetree_compatible"))
            } else {
                keyboard.get("devicetree_compatible") == Some(&Value::Null)
            },
            "hardware definition internal keyboard device-tree compatible is invalid",
        )?;
        require(
            seen.insert((text("bus"), text("vendor"), text("product"))),
            "hardware definition internal keyboards must be unique",
        )?;
    }
    Ok(())
}

/// `validate_dmi`: the DMI system identity a firmware-described board matches
/// on, kept out of a board that already has a device tree to name it.
fn validate_dmi(data: &Map<String, Value>) -> Result<()> {
    let Some(dmi) = data.get("dmi") else {
        return Ok(());
    };
    let Value::Object(dmi) = dmi else {
        return Err(Error::value(
            "hardware definition DMI has missing or unknown fields",
        ));
    };
    require(
        keys(dmi) == names(&DMI_FIELDS),
        "hardware definition DMI has missing or unknown fields",
    )?;
    for field in DMI_FIELDS {
        require(
            as_str(dmi.get(field).unwrap_or(&Value::Null)).is_some_and(|v| DMI_VALUE.is_match(v)),
            format!("hardware definition DMI {field} is invalid"),
        )?;
    }
    require(
        data.get("devicetree").unwrap_or(&Value::Null).is_null(),
        "hardware definition DMI requires a firmware-described board",
    )
}

/// `validate_target_imgref(value, root=root)`: a development image's bound
/// origin, a tilo-vm image under its dev flavour and repository policy, or a
/// signature-required release repository.
///
/// # Errors
/// Any other update origin.
pub fn validate_target_imgref(value: &Value, root: &Root) -> Result<()> {
    if let Some(bound) = development_origin(root)? {
        require(
            *value == bound,
            "update origin differs from immutable development origin",
        )?;
        return Ok(());
    }
    if as_str(value).is_some_and(|v| TILO_VM_REF.is_match(v)) {
        let flavour = root.trusted_path("usr/lib/tilo/image-flavour", true)?;
        let policy = root.trusted_path("etc/containers/policy.json", true)?;
        let flavour_size = fs::metadata(&flavour)
            .map_err(|e| Error::os(&e, &flavour))?
            .len();
        let flavour_bytes = if flavour_size > 16 {
            Vec::new()
        } else {
            fs::read(&flavour).map_err(|e| Error::os(&e, &flavour))?
        };
        require(
            flavour_size <= 16 && flavour_bytes == b"dev\n",
            "tilo-vm update origin requires the immutable dev flavour",
        )?;
        let policy_size = fs::metadata(&policy)
            .map_err(|e| Error::os(&e, &policy))?
            .len();
        require(
            policy_size <= 65536,
            "development container policy is too large",
        )?;
        let document = json_bytes(&fs::read(&policy).map_err(|e| Error::os(&e, &policy))?)?;
        let empty = json!({});
        let transports = get(&document, "transports")?.unwrap_or(&empty);
        let docker = get(transports, "docker")?.unwrap_or(&empty);
        let admission = get(docker, "registry.tilo.so/tilo/tilo-vm")?;
        let insecure = json!([{"type": "insecureAcceptAnything"}]);
        let signed = json!([{"type": "sigstoreSigned", "keyPath": "/usr/share/tilo/cosign.pub",
                             "signedIdentity": {"type": "matchRepository"}}]);
        require(
            admission == Some(&insecure) || admission == Some(&signed),
            "tilo-vm update origin requires its development repository policy",
        )?;
        if admission == Some(&signed) {
            root.trusted_path("usr/share/tilo/cosign.pub", true)?;
        }
        return Ok(());
    }
    require(
        as_str(value).is_some_and(|v| RELEASE_REF.is_match(v)),
        "TILO_TARGET_IMGREF must explicitly name a valid tag or sha256 digest in a signature-required Tilo repository (registry.tilo.so/tilo/tilo, or code.tilo.so/tilo/tilo for board images)",
    )
}

/// What `development_target` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopmentTarget {
    pub repository: String,
    pub registry: String,
    pub tls_required: bool,
}

/// `development_target(value, architecture)`: one explicitly selected
/// development repository, never production.
///
/// # Errors
/// Anything else.
pub fn development_target(
    value: &Value,
    architecture: Option<&Value>,
) -> Result<DevelopmentTarget> {
    let text = as_str(value).unwrap_or_default();
    let captures = DEVELOPMENT_REF.captures(text).ok_or_else(|| {
        Error::value("development origin must name an approved development repository with an explicit tag or digest")
    })?;
    let group = |i: usize| captures.get(i).map(|m| m.as_str());
    let host = group(1).unwrap_or_default();
    let port = group(2);
    let path = group(3).unwrap_or_default();
    let selector = group(4).unwrap_or_default();
    // `None` (JSON null) means no architecture was given.
    let architecture = architecture.filter(|a| !a.is_null());
    if let Some(architecture) = architecture {
        require(
            as_str(architecture).is_some_and(|a| ARCHITECTURES.contains(&a)),
            "unsupported development image architecture",
        )?;
    }
    let retained = host == "registry.tilo.so" && port.is_none() && path == "tilo/tilo-development";
    require(
        !((host == "registry.tilo.so"
            || host == "code.tilo.so"
            || path == "tilo/tilo-development")
            && !retained),
        "retained development repository must use its exact HTTPS registry identity",
    )?;
    let bad_part = host
        .split('.')
        .any(|part| part.is_empty() || part.starts_with('-') || part.ends_with('-'));
    require(
        host != "localhost" && !host.contains("..") && !bad_part,
        "development registry host is invalid or a production alias",
    )?;
    if retained && let Some(tag) = selector.strip_prefix(':') {
        let tagged = TAG_ARCHITECTURE
            .captures(tag)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());
        let matches_native = match (tagged, architecture) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(tagged), Some(architecture)) => as_str(architecture) == Some(tagged),
        };
        require(
            matches_native,
            "retained development update tag must identify the matching native architecture",
        )?;
    }
    if let Some(port) = port {
        let number: u32 = port.parse().unwrap_or(0);
        require(
            (1..=65535).contains(&number) && number.to_string() == port,
            "development registry port must be canonical and valid",
        )?;
    }
    let registry = port.map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
    Ok(DevelopmentTarget {
        repository: format!("{registry}/{path}"),
        registry,
        tls_required: host != "127.0.0.1",
    })
}

fn development_policy(repository: &str) -> Value {
    json!({"default": [{"type": "reject"}], "transports": {"docker": {repository: [
        {"type": "sigstoreSigned", "keyPath": "/usr/share/tilo/cosign.pub",
         "signedIdentity": {"type": "matchRepository"}}]}}})
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `development_origin(root)`: the bound development update origin of a
/// development image, `None` for any other image.
///
/// # Errors
/// A development image whose trust inputs do not bind each other.
pub fn development_origin(root: &Root) -> Result<Option<Value>> {
    if !root.present(DEVELOPMENT_PATH) {
        return Ok(None);
    }
    let dev = |name: &str| format!("{DEVELOPMENT_PATH}/{name}");
    let document = json_bytes(&root.read_bounded(&dev("origin.json"))?)?;
    let origin_fields = names(&["schema", "purpose", "target_update_ref", "manifest_sha256"]);
    let fields_ok = py::set_of(&document).is_some_and(|set| {
        set.iter().map(String::as_str).collect::<BTreeSet<_>>() == origin_fields
    });
    require(fields_ok, "invalid immutable development origin metadata")?;
    let schema = index(&document, "schema")?;
    require(
        py::as_int(schema) == Some(1) && index(&document, "purpose")? == "tilo-development",
        "invalid immutable development origin metadata",
    )?;
    let target = index(&document, "target_update_ref")?;
    let selected = development_target(target, None)?;
    let manifest_bytes = root.read_bounded(&dev("manifest.json"))?;
    let manifest_sha256 = index(&document, "manifest_sha256")?;
    require(
        *manifest_sha256 == Value::String(sha256_hex(&manifest_bytes)),
        "development origin public manifest binding differs",
    )?;
    let provenance = json_bytes(&root.read_bounded(&dev("provenance.json"))?)?;
    require(
        get(&provenance, "purpose")? == Some(&json!("tilo-development"))
            && get(&provenance, "manifest_sha256")? == Some(manifest_sha256),
        "development origin provenance differs",
    )?;
    let manifest = json_bytes(&manifest_bytes)?;
    let manifest_fields = names(&["schema", "purpose", "sha256"]);
    let fields_ok = py::set_of(&manifest).is_some_and(|set| {
        set.iter().map(String::as_str).collect::<BTreeSet<_>>() == manifest_fields
    });
    require(fields_ok, "invalid development public manifest")?;
    require(
        py::equals_number(index(&manifest, "schema")?, 1)
            && index(&manifest, "purpose")? == "tilo-development",
        "invalid development public manifest",
    )?;
    let identities = [
        "cosign.pub",
        "tpm2-pcr-signing.public.pem",
        "efi-signing.crt",
        "registry-owner.pub",
    ];
    let digests = index(&manifest, "sha256")?;
    let complete = py::set_of(digests).is_some_and(|set| {
        set.iter().map(String::as_str).collect::<BTreeSet<_>>() == names(&identities)
    });
    require(complete, "development public identities are incomplete")?;
    for name in identities {
        let expected = index(digests, name)?;
        require(
            *expected == Value::String(sha256_hex(&root.read_bounded(&dev(name))?)),
            "development public identity differs from manifest",
        )?;
    }
    for name in ["cosign.pub", "tpm2-pcr-signing.public.pem"] {
        require(
            root.read_bounded(&format!("usr/share/tilo/{name}"))?
                == root.read_bounded(&dev(name))?,
            "installed development key differs from bound public identity",
        )?;
    }
    let owner_bytes = root.read_bounded(&dev("registry-owner.pub"))?;
    let owner = py::strip_bytes(&owner_bytes);
    let installed_bytes = root.read_bounded("usr/share/tilo/trust/tilo-main-registry-owner.pub")?;
    let installed = py::strip_bytes(&installed_bytes);
    let owner_text = std::str::from_utf8(owner).unwrap_or_default();
    require(
        OWNER_KEY.is_match(owner_text)
            && owner.len() == 64
            && owner != [b'0'; 64]
            && installed == owner,
        "installed development registry owner differs from bound public identity",
    )?;
    require(
        json_bytes(&root.read_bounded("etc/containers/policy.json")?)?
            == development_policy(&selected.repository),
        "development update policy must require the bound signature identity and reject other origins",
    )?;
    let attachments = format!(
        "docker:\n  \"{}\":\n    use-sigstore-attachments: true\n",
        selected.repository
    );
    let transport = format!(
        "[[registry]]\nlocation = \"{}\"\ninsecure = {}\n",
        selected.registry, !selected.tls_required
    );
    require(
        root.read_bounded("etc/containers/registries.d/tilo-development.yaml")?
            == attachments.as_bytes()
            && root.read_bounded("etc/containers/registries.conf.d/99-tilo-development.conf")?
                == transport.as_bytes(),
        "development registry transport or signature attachment configuration differs",
    )?;
    Ok(Some(target.clone()))
}

/// `resolve_profile`: the pinned device tree this image's hardware profile
/// binds to kernel `kver`, or `None` when the image has no profile or its
/// model boots with the firmware's own device tree.
///
/// # Errors
/// A profile the module refuses, a kernel version or architecture that does
/// not match it, or a DTB that is missing, malformed or differs from its
/// pinned SHA-256.
pub fn resolve_profile(root: &Root, kver: &str, architecture: &str) -> Result<Option<PathBuf>> {
    let Some(data) = read_profile(root)? else {
        return Ok(None);
    };
    require(
        KERNEL_VERSION.is_match(kver),
        "invalid kernel version for hardware profile",
    )?;
    let definition = read_definition(&root.trusted_path(DEFINITION_PATH, true)?)?;
    let native = Value::from(architecture);
    require(
        ARCHITECTURES.contains(&architecture)
            && data.get("architecture") == Some(&native)
            && definition.get("architecture") == Some(&native),
        "hardware profile requires a matching native image architecture",
    )?;
    // The reviewed definition is copied into the sealed rootfs. It binds the
    // model to this kernel's DTB without model-specific branches in the
    // builder.
    let Some(devicetree) = definition.get("devicetree").and_then(Value::as_str) else {
        require(
            data.get("devicetree") == Some(&Value::Null)
                && data.get("devicetree_sha256") == Some(&Value::Null),
            "firmware-provided device tree profile must not override a DTB",
        )?;
        return Ok(None);
    };
    let relative = format!("usr/lib/modules/{kver}/dtb/{devicetree}");
    require(
        data.get("devicetree").and_then(Value::as_str) == Some(&format!("/{relative}")),
        "hardware profile device tree does not match this kernel and model",
    )?;
    let digest = data
        .get("devicetree_sha256")
        .and_then(Value::as_str)
        .filter(|digest| DTB_DIGEST.is_match(digest))
        .ok_or_else(|| Error::value("hardware profile has no valid pinned DTB SHA-256"))?;
    let dtb = root.trusted_path(&relative, true)?;
    let payload = fs::read(&dtb).map_err(|e| Error::os(&e, &dtb))?;
    let header = |at: usize| {
        payload
            .get(at..at.saturating_add(4))
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
    };
    require(
        payload.len() >= 40
            && header(0) == Some(0xD00D_FEED)
            && header(4).map(|size| size as usize) == Some(payload.len()),
        "hardware profile device tree has an invalid flattened-device-tree header",
    )?;
    require(
        sha256_hex(&payload) == digest,
        "hardware profile device tree does not match its pinned SHA-256",
    )?;
    Ok(Some(dtb))
}

/// The update origin an installer records when no image pins one.
pub const DEFAULT_TARGET_IMGREF: &str = "registry.tilo.so/tilo/tilo:latest";

/// `install_update_origin(root, override)`: the origin the installed system
/// follows. A hardware profile's reviewed reference wins and an `override`
/// may only repeat it; without a profile, the override, the development
/// image's bound origin or the official default, validated against `root`.
///
/// # Errors
/// A profile or origin the reader refuses, or an override that conflicts
/// with the profile.
pub fn install_update_origin(root: &Root, override_ref: &str) -> Result<String> {
    let bound_development = development_origin(root)?;
    let Some(profile) = read_profile(root)? else {
        let selected = if override_ref.is_empty() {
            bound_development.unwrap_or_else(|| Value::String(DEFAULT_TARGET_IMGREF.into()))
        } else {
            Value::String(override_ref.into())
        };
        validate_target_imgref(&selected, root)?;
        return as_str(&selected)
            .map(str::to_owned)
            .ok_or_else(|| Error::value("development origin is not a string"));
    };
    let bound = profile
        .get("target_update_ref")
        .and_then(as_str)
        .unwrap_or_default();
    require(
        override_ref.is_empty() || override_ref == bound,
        "TILO_TARGET_IMGREF conflicts with the image-owned hardware profile update reference",
    )?;
    Ok(bound.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_compiles() {
        for pattern in [
            &*TILO_VM_REF,
            &*RELEASE_REF,
            &*DEVELOPMENT_REF,
            &*TAG_ARCHITECTURE,
            &*OWNER_KEY,
            &*PROFILE_ID,
            &*PATH_PART,
            &*MODULE_NAME,
            &*KEYBOARD_ID,
            &*KEYBOARD_NAME,
            &*COMPATIBLE,
            &*KERNEL_ARG,
            &*DMI_VALUE,
            &*KERNEL_VERSION,
            &*DTB_DIGEST,
        ] {
            assert_ne!(pattern.as_str(), never().as_str());
        }
    }

    #[test]
    fn suffix_is_pathlib_suffix() {
        assert_eq!(suffix("qcom/board.dtb"), ".dtb");
        assert_eq!(suffix(".dtb"), "");
        assert_eq!(suffix("a.b.dtb"), ".dtb");
        assert_eq!(suffix("board"), "");
    }
}

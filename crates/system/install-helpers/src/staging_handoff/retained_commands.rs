//! Retained-source commands invoked by the authenticated root supervisor.
//!
//! Receipt stdin carries authenticated choices, not actor authority. The
//! supervisor owns authentication; no JSON field can substitute for it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use rustix::fs::{self, AtFlags, Mode, OFlags};
use serde_json::{Value, json};

use super::{Fail, HANDOFF_MAX_BYTES, Host, Result, Strict, canonical, lsblk_disk, number, refuse};

const RECEIPT_MAX_BYTES: usize = HANDOFF_MAX_BYTES + 32768;
const PREPARED_TRANSPORT: &str = "tilo-installer-prepared-setup-extract";

fn parse_receipt(input: impl Read) -> Result<Value> {
    let mut bytes = Vec::new();
    input
        .take((RECEIPT_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > RECEIPT_MAX_BYTES {
        return refuse("authenticated prepared choices exceed their bound");
    }
    // Strict rejects duplicate keys at every depth, including ignored metadata.
    let Strict(receipt) = serde_json::from_slice(&bytes)
        .map_err(|_| Fail::Refusal("authenticated prepared choices are malformed".into()))?;
    let allowed = [
        "schema",
        "identity_public_key",
        "setup_binding",
        "install_profile",
        "retained_home",
        "completed_home",
        "completed_setup",
        "completed_applications",
        "completed_applications_only",
        "computer_apps_scope",
        "planned_btrfs_resize",
        "pending_computer_applications_scope",
    ];
    if field(&receipt, "schema").as_u64() != Some(1)
        || receipt
            .as_object()
            .is_none_or(|map| map.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return refuse("authenticated prepared choices fields differ");
    }
    Ok(receipt)
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn present(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|v| !v.is_null())
}

fn prepared_transport(arguments: &[&str], receipt: Option<&Value>) -> Result<Vec<u8>> {
    // Resolve the installed sibling from the executable itself, never argv[0],
    // the working directory, PATH or a caller-controlled environment override.
    // In the image both helpers live in /usr/libexec.
    let executable = std::env::current_exe()
        .map_err(|_| Fail::Refusal("prepared setup transport is unavailable".into()))?;
    let transport = executable.with_file_name(PREPARED_TRANSPORT);
    let transport = transport
        .to_str()
        .filter(|_| transport.is_absolute())
        .ok_or_else(|| Fail::Refusal("prepared setup transport is unavailable".into()))?;
    let mut argv = vec![transport];
    argv.extend_from_slice(arguments);
    let input = receipt
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| Fail::Refusal("authenticated prepared choices are malformed".into()))?;
    let output = crate::process::run(&argv, input.as_deref(), Duration::from_secs(120))
        .map_err(|_| Fail::Refusal("prepared setup transport is unavailable".into()))?;
    if !output.success() || output.stdout.len() > 80 * 1024 {
        // Never echo private input or the child's diagnostics.
        return refuse("prepared setup transport validation failed");
    }
    Ok(output.stdout)
}

/// Extract choices already authenticated by the root CLI supervisor.
/// This JSON is public metadata, never proof of actor identity or mount authority.
/// The callback must parse the shared Migration model and call `validate_standalone`.
pub fn authenticated_retained_plan(
    input: impl Read,
    validate_migration: impl Fn(&Value) -> Result<Value>,
) -> Result<Option<Value>> {
    let receipt = parse_receipt(input)?;
    let completed = present(&receipt, "completed_home");
    let apps_only = match receipt.get("completed_applications_only") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return refuse("authenticated completed purpose is malformed"),
    };
    if apps_only && (completed || !present(&receipt, "completed_applications")) {
        return refuse("authenticated applications purpose differs");
    }
    if (completed || apps_only) != present(&receipt, "completed_setup") {
        return refuse("authenticated completed generation is incomplete");
    }
    if present(&receipt, "computer_apps_scope") && !apps_only {
        return refuse("authenticated Computer applications scope is unexpected");
    }
    if !completed && !apps_only && present(&receipt, "completed_applications") {
        return refuse("authenticated applications lack completed generation");
    }
    if completed || apps_only {
        let generation = prepared_transport(&["selected-generation"], None)?;
        let generation = std::str::from_utf8(&generation)
            .map_err(|_| Fail::Refusal("prepared setup generation is malformed".into()))?;
        prepared_transport(
            &["preflight-authenticated", "--source-dir", generation.trim()],
            Some(&receipt),
        )?;
        if (present(&receipt, "retained_home") && !apps_only)
            || (completed
                && field(field(&receipt, "completed_home"), "setup_binding")
                    != field(&receipt, "setup_binding"))
        {
            return refuse("authenticated completed Home lineage differs");
        }
    }
    // Use the existing native closed parser for pending Computer-app scope,
    // without claiming a completed archive or opening a source directory.
    let mut metadata = serde_json::Map::new();
    metadata.insert("schema".into(), json!(1));
    for key in [
        "identity_public_key",
        "setup_binding",
        "retained_home",
        "pending_computer_applications_scope",
    ] {
        if let Some(value) = receipt.get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    prepared_transport(
        &["preflight-authenticated", "--source-dir", "/nonexistent"],
        Some(&Value::Object(metadata)),
    )?;
    if !present(&receipt, "retained_home") {
        return Ok(None);
    }
    let mut plan = validate_migration(field(&receipt, "retained_home"))?;
    if present(&receipt, "planned_btrfs_resize") {
        validate_resize(&plan, field(&receipt, "planned_btrfs_resize"))?;
    }
    let shared = field(&plan, "home_policy") == "access-read-write"
        && field(field(&plan, "categories"), "home") == true;
    let apps = plan.get("home_policy").is_none_or(|p| p == "copy")
        && field(field(&plan, "categories"), "home") == false
        && field(field(&plan, "categories"), "flatpak") == true
        && plan.get("flatpak_inventory").is_some();
    if !(shared || apps) {
        return refuse("authenticated retained plan lacks shared-files or independent-app consent");
    }
    if present(&receipt, "pending_computer_applications_scope") {
        plan.as_object_mut()
            .ok_or_else(|| Fail::Refusal("authenticated retained plan is malformed".into()))?
            .insert(
                "pending_computer_applications_scope".into(),
                field(&receipt, "pending_computer_applications_scope").clone(),
            );
    }
    // Python canonical adds one newline. serde's UTF-8 output can be shorter
    // than ensure_ascii JSON; measure its escaped canonical representation.
    if super::canonical(&plan).len() > HANDOFF_MAX_BYTES {
        return refuse("authenticated original-files plan exceeds reboot delivery bound");
    }
    Ok(Some(plan))
}

fn validate_resize(plan: &Value, resize: &Value) -> Result<()> {
    let keys = [
        "schema",
        "original_plan_sha256",
        "reviewed_plan_sha256",
        "reviewed_plan",
        "filesystem_uuid",
        "partition_guid",
        "device_id",
        "from_bytes",
        "to_bytes",
        "new_last_lba",
    ];
    let volume = field(field(plan, "home"), "volume");
    let first = bounded(volume, "first_lba")?;
    let last = bounded(volume, "last_lba")?;
    let sector = bounded(volume, "sector_bytes")?;
    let bytes = last
        .checked_sub(first)
        .and_then(|length| length.checked_add(1))
        .and_then(|length| length.checked_mul(sector))
        .ok_or_else(|| Fail::Refusal("authenticated resized source differs".into()))?;
    let from = field(resize, "from_bytes").as_u64();
    let to = field(resize, "to_bytes").as_u64();
    if resize
        .as_object()
        .is_none_or(|map| map.len() != keys.len() || keys.iter().any(|key| !map.contains_key(*key)))
        || field(resize, "schema").as_u64() != Some(1)
        || field(resize, "device_id").as_u64() != Some(1)
        || field(volume, "filesystem") != "btrfs"
        || field(resize, "filesystem_uuid") != field(volume, "filesystem_uuid")
        || field(resize, "partition_guid") != field(volume, "partition_guid")
        || field(resize, "new_last_lba") != field(volume, "last_lba")
        || to.map(i128::from) != Some(bytes)
        || !matches!((from, to), (Some(from), Some(to)) if from > to)
    {
        return refuse("authenticated resized source differs");
    }
    Ok(())
}

fn bounded(value: &Value, key: &str) -> Result<i128> {
    super::int(field(value, key), key, 0)
}

/// Re-attest geometry before any target writes.
pub fn preflight_retained(
    host: &dyn Host,
    plan: Option<&Value>,
    disk: &str,
    writes: &[(i128, i128)],
) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    if field(plan, "home_policy") == "access-read-write" {
        let (minimum, maximum) = regular_user_range()?;
        let uid = bounded(field(plan, "home"), "original_uid")?;
        if uid < i128::from(minimum) || uid > i128::from(maximum) {
            return refuse("retained source UID is outside target regular-account policy");
        }
    }
    if writes.is_empty() {
        return refuse("installation write extents are missing");
    }
    let node = lsblk_disk(host, disk)?;
    let size = number(&node, "size")?;
    if writes.iter().any(|&(start, length)| {
        start < 0 || length <= 0 || start.checked_add(length).is_none_or(|end| end > size)
    }) {
        return refuse("installation write extent is outside the target disk");
    }
    let source = field(field(plan, "home"), "volume");
    let text = |value: &Value| value.as_str().unwrap_or_default().to_lowercase();
    if text(node.get("ptuuid").unwrap_or(&Value::Null)) != text(field(source, "disk_guid")) {
        return Ok(());
    }
    let sector = bounded(source, "sector_bytes")?;
    if size != bounded(source, "disk_bytes")? || number(&node, "log-sec")? != sector {
        return refuse("retained source disk geometry changed");
    }
    let matches: Vec<&Value> = node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| text(field(part, "partuuid")) == text(field(source, "partition_guid")))
        .collect();
    if matches.len() != 1 {
        return refuse("retained source partition is missing or ambiguous");
    }
    let part = matches
        .first()
        .and_then(|part| part.as_object())
        .ok_or_else(|| Fail::Refusal("retained source partition is malformed".into()))?;
    let geometry_error = || Fail::Refusal("retained source filesystem or extent changed".into());
    let first = bounded(source, "first_lba")?
        .checked_mul(sector)
        .ok_or_else(geometry_error)?;
    let end = bounded(source, "last_lba")?
        .checked_add(1)
        .and_then(|last| last.checked_mul(sector))
        .ok_or_else(geometry_error)?;
    let length = end.checked_sub(first).ok_or_else(geometry_error)?;
    if number(part, "start")?.checked_mul(512) != Some(first)
        || number(part, "size")? != length
        || text(part.get("uuid").unwrap_or(&Value::Null)) != text(field(source, "filesystem_uuid"))
        || part.get("fstype") != source.get("filesystem")
    {
        return refuse("retained source filesystem or extent changed");
    }
    if writes.iter().any(|&(start, length)| {
        start < end && start.checked_add(length).is_none_or(|end| end > first)
    }) {
        return refuse("installation would modify the retained source filesystem");
    }
    Ok(())
}

/// Read the target regular-account policy, never the retained source's policy.
///
/// # Errors
/// Unsafe ownership/type/mode, excessive size, duplicate or malformed ranges.
pub fn regular_user_range() -> Result<(u32, u32)> {
    let fd = rustix::fs::open(
        "/etc/login.defs",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let file = File::from(fd);
    let facts = file.metadata()?;
    if !facts.is_file() || facts.uid() != 0 || facts.mode() & 0o022 != 0 || facts.len() > 65536 {
        return refuse("installed regular-account policy is unsafe");
    }
    let mut contents = String::new();
    file.take(65537).read_to_string(&mut contents)?;
    if contents.len() > 65536 {
        return refuse("installed regular-account policy exceeds its size bound");
    }
    parse_user_range(&contents)
}

fn parse_user_range(contents: &str) -> Result<(u32, u32)> {
    let (mut minimum, mut maximum) = (1000_u32, 60_000);
    let (mut seen_minimum, mut seen_maximum) = (false, false);
    for line in contents.lines() {
        let mut fields = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let (value, seen) = match fields.next() {
            Some("UID_MIN") => (&mut minimum, &mut seen_minimum),
            Some("UID_MAX") => (&mut maximum, &mut seen_maximum),
            _ => continue,
        };
        let number = fields.next().unwrap_or_default();
        if *seen
            || fields.next().is_some()
            || number.is_empty()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return refuse("installed regular-account range is malformed");
        }
        *value = number
            .parse()
            .map_err(|_| Fail::Refusal("installed regular-account range is malformed".into()))?;
        *seen = true;
    }
    if minimum < 1000 || minimum > maximum || maximum == u32::MAX {
        return refuse("installed regular-account range is invalid");
    }
    Ok((minimum, maximum))
}

/// Open every component relative to a pinned descriptor, never following links.
fn open_directory_chain(path: &Path) -> Result<OwnedFd> {
    let bytes = path.as_os_str().as_bytes();
    let Some(relative) = bytes.strip_prefix(b"/") else {
        return refuse("metadata delivery directory is not absolute");
    };
    let parts: Vec<_> = relative.split(|&byte| byte == b'/').collect();
    if parts.iter().any(|part| matches!(*part, b"" | b"." | b"..")) {
        return refuse("metadata delivery directory is not normalized");
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = fs::open("/", flags, Mode::empty())?;
    for part in parts {
        directory = fs::openat(
            &directory,
            std::ffi::OsStr::from_bytes(part),
            flags,
            Mode::empty(),
        )?;
    }
    Ok(directory)
}

fn safe_directory(directory: &OwnedFd, message: &str) -> Result<()> {
    let facts = fs::fstat(directory)?;
    if facts.st_uid != rustix::process::geteuid().as_raw() || facts.st_mode & 0o022 != 0 {
        return refuse(message);
    }
    Ok(())
}

fn read_plan_at(
    directory: &OwnedFd,
    validate_migration: &impl Fn(&Value) -> Result<Value>,
) -> Result<Option<Value>> {
    let fd = match fs::openat(
        directory,
        "migration-plan.json",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let file = File::from(fd);
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != rustix::process::geteuid().as_raw()
        || facts.mode() & 0o077 != 0
        || facts.len() > HANDOFF_MAX_BYTES as u64
    {
        return refuse("migration metadata is not a bounded private owner-controlled file");
    }
    let mut bytes = Vec::new();
    file.take((HANDOFF_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > HANDOFF_MAX_BYTES {
        return refuse("migration metadata exceeds its size bound");
    }
    let Strict(value) = serde_json::from_slice(&bytes)
        .map_err(|_| Fail::Refusal("migration metadata is not valid JSON".into()))?;
    validate_migration(&value).map(Some)
}

/// Atomically publish private migration intent without overwriting existing intent.
pub fn deliver_retained(
    plan: Option<&Value>,
    target_var: &Path,
    validate_migration: impl Fn(&Value) -> Result<Value>,
) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    let data = canonical(plan);
    let mut directory = open_directory_chain(target_var)?;
    safe_directory(&directory, "installed /var custody is unsafe")?;
    for component in ["lib", "tilo"] {
        match fs::mkdirat(&directory, component, Mode::from_raw_mode(0o755)) {
            Ok(()) => fs::fsync(&directory)?,
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        directory = fs::openat(
            &directory,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        safe_directory(
            &directory,
            "installed original-files metadata namespace is unsafe",
        )?;
    }
    if let Some(existing) = read_plan_at(&directory, &validate_migration)? {
        if canonical(&existing) != data {
            return refuse("installed migration intent conflicts with authenticated setup");
        }
        return Ok(());
    }
    // /proc/self/fd resolves only our pinned, still-owned directory descriptor.
    // tempfile provides an unpredictable O_EXCL, mode-0600 temporary and removes
    // it on every failure path; publication itself is descriptor-relative.
    let pinned = format!("/proc/self/fd/{}", directory.as_fd().as_raw_fd());
    let mut temporary = tempfile::Builder::new()
        .prefix(".retained-")
        .tempfile_in(pinned)?;
    temporary.write_all(&data)?;
    temporary.as_file().sync_all()?;
    let name = temporary
        .path()
        .file_name()
        .ok_or_else(|| Fail::Refusal("retained temporary name is missing".into()))?;
    fs::linkat(
        &directory,
        name,
        &directory,
        "migration-plan.json",
        AtFlags::empty(),
    )?;
    fs::fsync(&directory)?;
    temporary.close()?;
    Ok(())
}

/// Run a retained CLI command with bounded stdin.
///
/// The validator must deserialize the shared Migration model and invoke its
/// `validate_standalone()` method, returning the original JSON after validation.
/// This explicit callback avoids coupling these commands to handoff geometry.
pub fn run(
    host: &dyn Host,
    command: &str,
    args: &[OsString],
    validate_migration: impl Fn(&Value) -> Result<Value>,
) -> Result<()> {
    if !matches!(command, "preflight-retained" | "deliver-retained") {
        return refuse("unknown retained command");
    }
    let mut disk = None;
    let mut target_var = None;
    let mut starts = Vec::new();
    let mut sizes = Vec::new();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.as_os_str();
        let bytes = argument.as_bytes();
        let mut parts = bytes.splitn(2, |&byte| byte == b'=');
        let flag = parts.next().unwrap_or_default();
        let inline = parts.next().map(std::ffi::OsStr::from_bytes);
        let value = if let Some(value) = inline {
            value
        } else {
            arguments
                .next()
                .map(OsString::as_os_str)
                .ok_or_else(|| Fail::Refusal("retained command option requires a value".into()))?
        };
        match (command, flag) {
            ("preflight-retained", b"--disk") => disk = Some(value),
            ("deliver-retained", b"--target-var") => target_var = Some(value),
            ("preflight-retained", b"--write-start" | b"--write-size") => {
                let number = value
                    .to_str()
                    .and_then(|value| value.parse::<i128>().ok())
                    .ok_or_else(|| {
                        Fail::Refusal("installation write extent is not an integer".into())
                    })?;
                if flag == b"--write-start" {
                    starts.push(number);
                } else {
                    sizes.push(number);
                }
            }
            _ => return refuse("unknown retained command option"),
        }
    }
    if command == "deliver-retained" {
        let target_var = target_var
            .ok_or_else(|| Fail::Refusal("retained delivery requires --target-var".into()))?;
        let plan = authenticated_retained_plan(std::io::stdin().lock(), &validate_migration)?;
        deliver_retained(plan.as_ref(), Path::new(target_var), validate_migration)
    } else {
        let disk = disk
            .and_then(|disk| disk.to_str())
            .ok_or_else(|| Fail::Refusal("retained preflight requires --disk".into()))?;
        let plan = authenticated_retained_plan(std::io::stdin().lock(), validate_migration)?;
        if plan.is_some() && starts.len() != sizes.len() {
            return refuse("installation write extents are missing");
        }
        let writes: Vec<_> = starts.into_iter().zip(sizes).collect();
        preflight_retained(host, plan.as_ref(), disk, &writes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn validated(value: &Value) -> Result<Value> {
        if !value.is_object() {
            return refuse("migration plan must be an object");
        }
        Ok(value.clone())
    }

    #[test]
    fn receipt_is_bounded_and_rejects_duplicate_keys_at_every_depth() {
        for bytes in [
            br#"{"schema":1,"schema":1}"#.as_slice(),
            br#"{"schema":1,"retained_home":{"home":{"inode":1,"inode":2}}}"#,
            br#"{"schema":1,"unknown":null}"#,
            br#"{"schema":true}"#,
            br#"{"schema":1,"retained_home":NaN}"#,
            b"\xff",
        ] {
            assert!(parse_receipt(bytes).is_err());
        }
        let bytes = vec![b' '; RECEIPT_MAX_BYTES + 1];
        assert!(
            parse_receipt(bytes.as_slice())
                .unwrap_err()
                .to_string()
                .contains("bound")
        );
    }

    #[test]
    fn completed_receipt_consistency_is_checked_before_transport() {
        for receipt in [
            json!({"schema":1, "completed_applications_only":null}),
            json!({"schema":1, "completed_applications_only":true}),
            json!({"schema":1, "completed_home":{}, "completed_setup":null}),
            json!({"schema":1, "completed_applications":{}}),
            json!({"schema":1, "computer_apps_scope":{}}),
        ] {
            assert!(
                authenticated_retained_plan(
                    serde_json::to_vec(&receipt).unwrap().as_slice(),
                    validated,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn resize_requires_exact_filesystem_and_geometry() {
        let plan = json!({"home":{"volume":{
            "first_lba":2048, "last_lba":4095, "sector_bytes":512,
            "filesystem":"btrfs", "filesystem_uuid":"fs", "partition_guid":"part"
        }}});
        let resize = json!({
            "schema":1, "device_id":1, "original_plan_sha256":null,
            "reviewed_plan_sha256":null, "reviewed_plan":null,
            "filesystem_uuid":"fs", "partition_guid":"part",
            "from_bytes":2_097_152, "to_bytes":1_048_576, "new_last_lba":4095
        });
        assert!(validate_resize(&plan, &resize).is_ok());
        for (key, value) in [
            ("to_bytes", json!(1_048_575)),
            ("from_bytes", json!(1_048_576)),
            ("new_last_lba", json!(4096)),
            ("device_id", json!(2)),
            ("filesystem_uuid", json!("other")),
            ("partition_guid", json!("other")),
        ] {
            let mut changed = resize.clone();
            *changed.get_mut(key).unwrap() = value;
            assert!(validate_resize(&plan, &changed).is_err(), "{key}");
        }
    }

    struct Disk(Value);

    impl Host for Disk {
        fn lsblk(&self, _disk: &str) -> Result<Value> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn preflight_preserves_sector_geometry_and_exclusive_write_boundaries() {
        for sector in [512_i128, 4096] {
            let first = 2048 * sector;
            let end = 4096 * sector;
            let plan = json!({"home_policy":"copy","home":{"volume":{
                "disk_guid":"disk", "disk_bytes":134_217_728, "sector_bytes":u64::try_from(sector).unwrap(),
                "partition_guid":"part", "first_lba":2048, "last_lba":4095,
                "filesystem_uuid":"fs", "filesystem":"btrfs"
            }}});
            let host = Disk(json!({"blockdevices":[{
                "name":"/dev/test", "type":"disk", "pttype":"gpt", "ptuuid":"DISK",
                "size":134_217_728, "log-sec":u64::try_from(sector).unwrap(), "children":[{
                    "partuuid":"PART", "parttype":"data", "uuid":"FS", "fstype":"btrfs",
                    "start":u64::try_from(first / 512).unwrap(), "size":u64::try_from(end.checked_sub(first).unwrap()).unwrap()
                }]
            }]}));
            assert!(
                preflight_retained(
                    &host,
                    Some(&plan),
                    "/dev/test",
                    &[(first - 512, 512), (end, 512)]
                )
                .is_ok()
            );
            for writes in [
                vec![],
                vec![(first - 512, 513)],
                vec![(end - 1, 1)],
                vec![(-1, 512)],
                vec![(0, 0)],
                vec![(i128::MAX, 1)],
            ] {
                assert!(preflight_retained(&host, Some(&plan), "/dev/test", &writes).is_err());
            }
            let mut changed = host.0.clone();
            *changed
                .get_mut("blockdevices")
                .and_then(|devices| devices.get_mut(0))
                .and_then(|device| device.get_mut("children"))
                .and_then(|children| children.get_mut(0))
                .and_then(|child| child.get_mut("start"))
                .unwrap() = json!(u64::try_from(first / 512 + 1).unwrap());
            assert!(
                preflight_retained(&Disk(changed), Some(&plan), "/dev/test", &[(0, 512)]).is_err()
            );
        }
    }

    #[test]
    fn delivery_is_private_idempotent_and_never_replaces_conflicting_intent() {
        let temporary = tempfile::tempdir().unwrap();
        let plan = json!({"schema":1,"intent":"first"});
        deliver_retained(Some(&plan), temporary.path(), validated).unwrap();
        let path = temporary.path().join("lib/tilo/migration-plan.json");
        assert_eq!(std::fs::read(&path).unwrap(), canonical(&plan));
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        deliver_retained(Some(&plan), temporary.path(), validated).unwrap();
        let different = json!({"schema":1,"intent":"second"});
        assert!(deliver_retained(Some(&different), temporary.path(), validated).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), canonical(&plan));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn delivery_refuses_symlinked_or_public_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(destination.path(), temporary.path().join("lib")).unwrap();
        let plan = json!({"schema":1});
        assert!(deliver_retained(Some(&plan), temporary.path(), validated).is_err());
        assert_eq!(std::fs::read_dir(destination.path()).unwrap().count(), 0);

        let clean = tempfile::tempdir().unwrap();
        deliver_retained(Some(&plan), clean.path(), validated).unwrap();
        let path = clean.path().join("lib/tilo/migration-plan.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(deliver_retained(Some(&plan), clean.path(), validated).is_err());
    }

    #[test]
    fn directory_path_requires_normalized_absolute_components() {
        for path in [
            "relative",
            "/",
            "//var",
            "/var/",
            "/var/../var",
            "/var/./lib",
        ] {
            assert!(open_directory_chain(Path::new(path)).is_err(), "{path}");
        }
    }
}

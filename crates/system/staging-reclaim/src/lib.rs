//! `tilo-staging-reclaim`: returns the temporary installer space to Tilo.
//!
//! A USB-free installation leaves two partitions behind the new Tilo root:
//! the staging ESP and staging root the live installer booted from. The
//! installer recorded their exact identities in
//! `/var/lib/tilo/staging-reclaim.json`. Once the installed system has reached
//! a real user session, this removes them and grows the Tilo root (GPT entry,
//! LUKS mapping, filesystem) into the freed space.
//!
//! Every step is decided from the disk as it is now, so a crash at any point
//! is resumed by the next run: a staging partition is either still present
//! with its exact recorded identity or absent with its extent unallocated; the
//! root either still ends at its recorded end or already ends at the grown
//! end. Anything else is a mismatch, and a mismatch leaves every partition
//! untouched and says why.
//!
//! The lines `tilo-staging-reclaim: done` and `tilo-staging-reclaim: refused:
//! …` are stable markers the unit mirrors to the console. Exit status 0 when
//! done or when there is nothing to do yet, 1 otherwise.
//!
//! Everything it asks of the machine (the record's path, the physical root
//! mount, the disk lock, the external tools, the log) goes through [`Host`],
//! whose provided methods are the production behaviour; nothing is read from
//! the environment.

mod py;
pub mod record;

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use advisory_lock::FileLock;
use serde_json::Value;

pub use record::{Extent, Record};

pub const RECORD: &str = "/var/lib/tilo/staging-reclaim.json";
pub const ROOT_MOUNT: &str = "/sysroot";
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// The Tilo staging root type; only the live installer UKI resolves it.
pub const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
pub const TILO_ROOT_TYPE: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";

/// How a run stopped, by what the original raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The disk does not look exactly like the record; nothing was touched.
    Mismatch(String),
    /// A step failed; the next run resumes from the disk as it is.
    Incomplete(String),
    /// A tool answered in a shape the original did not expect (it stopped
    /// with a traceback).
    Crash(String),
}

impl Error {
    /// An operating-system error on `path`, worded as Python worded it.
    #[must_use]
    pub fn os(error: &std::io::Error, path: &Path) -> Self {
        let text = error.to_string();
        let reason = text.split(" (os error").next().unwrap_or(&text);
        Self::Incomplete(format!(
            "[Errno {}] {reason}: '{}'",
            error.raw_os_error().unwrap_or(0),
            path.display()
        ))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch(message) | Self::Incomplete(message) | Self::Crash(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

/// What a tool did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The machine. Production uses every provided method.
pub trait Host {
    /// Where the installer's record is.
    fn record(&self) -> PathBuf {
        PathBuf::from(RECORD)
    }

    /// The physical root whose filesystem grows.
    fn root_mount(&self) -> PathBuf {
        PathBuf::from(ROOT_MOUNT)
    }

    /// Whether a loop device counts as a disk (never in production).
    fn allow_loop(&self) -> bool {
        false
    }

    /// Takes the disk's exclusive lock (`flock` on its device node), held
    /// until the returned descriptor is dropped.
    ///
    /// # Errors
    /// The node cannot be opened or locked.
    fn lock(&self, disk: &str) -> Result<Option<FileLock>, Error> {
        let node = std::fs::File::open(disk).map_err(|e| Error::os(&e, Path::new(disk)))?;
        FileLock::exclusive(node)
            .map(Some)
            .map_err(|e| Error::os(&e, Path::new(disk)))
    }

    /// Runs a tool found on `PATH`, with `input` on stdin (or none).
    ///
    /// # Errors
    /// The tool could not be started, or it wrote output that is not UTF-8.
    fn run(&self, argv: &[&str], input: Option<&str>) -> Result<Outcome, Error> {
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| Error::Crash("no command".to_owned()))?;
        let mut command = Command::new(program);
        command
            .args(rest)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| Error::os(&e, Path::new(program)))?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| Error::os(&e, Path::new(program)))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| Error::os(&e, Path::new(program)))?;
        let decode =
            |bytes: Vec<u8>| String::from_utf8(bytes).map_err(|e| Error::Incomplete(e.to_string()));
        Ok(Outcome {
            code: output.status.code().unwrap_or_else(|| {
                use std::os::unix::process::ExitStatusExt;
                output.status.signal().map_or(-1, i32::saturating_neg)
            }),
            stdout: decode(output.stdout)?,
            stderr: decode(output.stderr)?,
        })
    }

    /// Writes one `tilo-staging-reclaim: …` line to stdout, flushed.
    fn log(&self, message: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "tilo-staging-reclaim: {message}");
        let _ = out.flush();
    }
}

/// The machine itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

/// `run(argv)` with `check=True`: a non-zero exit stops the run.
fn checked(host: &dyn Host, argv: &[&str]) -> Result<Outcome, Error> {
    let outcome = host.run(argv, None)?;
    if outcome.code != 0 {
        return Err(Error::Incomplete(format!(
            "{} failed: {}",
            argv.join(" "),
            outcome.stderr.trim()
        )));
    }
    Ok(outcome)
}

fn json(text: &str) -> Result<Value, Error> {
    serde_json::from_str(text).map_err(|e| Error::Incomplete(e.to_string()))
}

/// `value.get(key)`: `AttributeError` (a crash) on anything but an object.
fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, Error> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        other => Err(Error::Crash(format!(
            "'{}' object has no attribute 'get'",
            kind(other)
        ))),
    }
}

/// `value[key]`: `KeyError` (incomplete) when absent, a crash on a non-object.
fn index<'a>(value: &'a Value, key: &str) -> Result<&'a Value, Error> {
    get(value, key)?.ok_or_else(|| Error::Incomplete(format!("'{key}'")))
}

const fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn int(value: Option<&Value>) -> Result<i128, Error> {
    py::int_of(value).map_err(|message| {
        if message.starts_with("int() argument") {
            Error::Crash(message)
        } else {
            Error::Incomplete(message)
        }
    })
}

fn list(value: Option<&Value>) -> Result<Vec<Value>, Error> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(other) => Err(Error::Crash(format!(
            "'{}' object is not iterable",
            kind(other)
        ))),
    }
}

/// Whether a real person is signed in: a `user`-class session of a uid in
/// 1000..60000 that is active or online.
///
/// # Errors
/// `loginctl` cannot be started.
pub fn user_session_exists(host: &dyn Host) -> Result<bool, Error> {
    let listing = host.run(&["loginctl", "list-sessions", "--no-legend"], None)?;
    if listing.code != 0 {
        return Ok(false);
    }
    for line in listing.stdout.lines() {
        let Some(id) = line.split_whitespace().next() else {
            continue;
        };
        let shown = host
            .run(
                &[
                    "loginctl",
                    "show-session",
                    id,
                    "-p",
                    "Class",
                    "-p",
                    "Uid",
                    "-p",
                    "State",
                ],
                None,
            )?
            .stdout;
        let mut values = std::collections::HashMap::new();
        for item in shown.lines() {
            if let Some((key, value)) = item.split_once('=') {
                values.insert(key.to_owned(), value.to_owned());
            }
        }
        let uid = values
            .get("Uid")
            .filter(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|uid| uid.parse::<u64>().ok());
        if values.get("Class").map(String::as_str) == Some("user")
            && uid.is_some_and(|uid| (1000..60000).contains(&uid))
            && matches!(
                values.get("State").map(String::as_str),
                Some("active" | "online")
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The one disk whose GPT GUID and size match the record.
///
/// # Errors
/// No such disk, more than one, or a tool failure.
pub fn find_disk(host: &dyn Host, record: &Record) -> Result<String, Error> {
    let listing = json(
        &checked(
            host,
            &[
                "lsblk",
                "-J",
                "-b",
                "-p",
                "-d",
                "-o",
                "NAME,TYPE,PTTYPE,PTUUID,SIZE",
            ],
        )?
        .stdout,
    )?;
    let kinds: &[&str] = if host.allow_loop() {
        &["disk", "loop"]
    } else {
        &["disk"]
    };
    let mut candidates = Vec::new();
    for item in list(get(&listing, "blockdevices")?)? {
        if get(&item, "type")?
            .and_then(Value::as_str)
            .is_some_and(|t| kinds.contains(&t))
        {
            candidates.push(item);
        }
    }
    for item in &mut candidates {
        // lsblk reads PTTYPE/PTUUID from udev; read the GPT itself when the
        // udev database has nothing (containers, very early boot).
        if py::is_falsy(get(item, "ptuuid")?) || py::is_falsy(get(item, "pttype")?) {
            let name = py::str_of(Some(index(item, "name")?));
            let probe = host.run(&["sfdisk", "--json", &name], None)?;
            if probe.code == 0 {
                let parsed = json(&probe.stdout)?;
                let empty = Value::Object(serde_json::Map::new());
                let table = get(&parsed, "partitiontable")?.unwrap_or(&empty).clone();
                for (field, source) in [("ptuuid", "id"), ("pttype", "label")] {
                    let current = get(item, field)?.cloned();
                    let value = if py::is_falsy(current.as_ref()) {
                        get(&table, source)?.cloned().unwrap_or(Value::Null)
                    } else {
                        current.unwrap_or(Value::Null)
                    };
                    if let Value::Object(map) = item {
                        map.insert(field.to_owned(), value);
                    }
                }
            }
        }
    }
    let mut disks = Vec::new();
    for item in candidates {
        if py::str_or_empty(get(&item, "ptuuid")?).to_lowercase() == record.disk_guid {
            disks.push(item);
        }
    }
    let [disk] = disks.as_slice() else {
        return Err(Error::Mismatch(format!(
            "expected exactly one disk with GPT GUID {}, found {}",
            record.disk_guid,
            disks.len()
        )));
    };
    let size = get(disk, "size")?;
    let size = if py::is_falsy(size) { 0 } else { int(size)? };
    if py::str_or_empty(get(disk, "pttype")?).to_lowercase() != "gpt"
        || size != record.disk_size_bytes
    {
        return Err(Error::Mismatch(format!(
            "disk {} differs from the recorded disk",
            py::str_of(get(disk, "name")?)
        )));
    }
    match index(disk, "name")? {
        Value::String(name) => Ok(name.clone()),
        other => Err(Error::Crash(format!("expected a device name, got {other}"))),
    }
}

/// One partition as the GPT has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub node: String,
    pub partuuid: String,
    pub kind: String,
    pub first_lba: i128,
    pub last_lba: i128,
}

impl Partition {
    fn overlaps(&self, first: i128, last: i128) -> bool {
        self.first_lba <= last && first <= self.last_lba
    }
}

/// The disk's GPT, required to be the recorded one.
///
/// # Errors
/// Another table, another sector size, or a tool failure.
pub fn partition_table(
    host: &dyn Host,
    disk: &str,
    record: &Record,
) -> Result<Vec<Partition>, Error> {
    let parsed = json(&checked(host, &["sfdisk", "--json", disk])?.stdout)?;
    let table = index(&parsed, "partitiontable")?;
    let id = get(table, "id")?.map_or_else(String::new, |id| py::str_of(Some(id)));
    if get(table, "label")?.and_then(Value::as_str) != Some("gpt")
        || id.to_lowercase() != record.disk_guid
    {
        return Err(Error::Mismatch(format!(
            "partition table of {disk} is not the recorded GPT"
        )));
    }
    let sector = get(table, "sectorsize")?;
    if (match sector {
        None => 0,
        Some(value) => int(Some(value))?,
    }) != record.sector_bytes
    {
        return Err(Error::Mismatch(format!(
            "sector size of {disk} differs from the record"
        )));
    }
    let mut parts = Vec::new();
    for part in list(get(table, "partitions")?)? {
        let start = int(Some(index(&part, "start")?))?;
        let size = int(Some(index(&part, "size")?))?;
        let text = |key: &str| -> Result<String, Error> {
            Ok(get(&part, key)?
                .map_or_else(String::new, |v| py::str_of(Some(v)))
                .to_lowercase())
        };
        parts.push(Partition {
            node: py::str_of(Some(index(&part, "node")?)),
            partuuid: text("uuid")?,
            kind: text("type")?,
            first_lba: start,
            last_lba: start.saturating_add(size).saturating_sub(1),
        });
    }
    Ok(parts)
}

/// The number at the end of a partition node (`/dev/vda6` is 6).
///
/// # Errors
/// A node that does not end in digits.
pub fn partition_number(node: &str) -> Result<u32, Error> {
    let trimmed = node.strip_suffix('\n').unwrap_or(node);
    let digits: String = trimmed
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse()
        .map_err(|_| Error::Mismatch(format!("cannot derive a partition number from {node}")))
}

/// A node's holders (device-mapper children) and its mount points.
fn holders_and_mounts(host: &dyn Host, node: &str) -> Result<(Vec<Value>, usize), Error> {
    let tree = json(
        &checked(
            host,
            &["lsblk", "-J", "-p", "-o", "NAME,TYPE,MOUNTPOINTS", node],
        )?
        .stdout,
    )?;
    let devices = index(&tree, "blockdevices")?;
    let device = match devices {
        Value::Array(items) => items
            .first()
            .ok_or_else(|| Error::Crash("list index out of range".to_owned()))?,
        other => {
            return Err(Error::Crash(format!(
                "'{}' object is not subscriptable",
                kind(other)
            )));
        }
    };
    let mounts = get(device, "mountpoints")?;
    let points = if py::is_falsy(mounts) {
        0
    } else {
        list(mounts)?
            .iter()
            .filter(|point| !py::is_falsy(Some(point)))
            .count()
    };
    let children = get(device, "children")?;
    let children = if py::is_falsy(children) {
        Vec::new()
    } else {
        list(children)?
    };
    Ok((children, points))
}

/// The present staging partition, or `None` when it is already gone.
fn staged_state(
    host: &dyn Host,
    parts: &[Partition],
    expected: &Extent,
    part_type: &str,
    label: &str,
) -> Result<Option<Partition>, Error> {
    let same: Vec<&Partition> = parts
        .iter()
        .filter(|p| p.partuuid == expected.partuuid)
        .collect();
    let part = match same.as_slice() {
        [] => return Ok(None),
        [part] => *part,
        _ => {
            return Err(Error::Mismatch(format!(
                "{label} GUID appears more than once"
            )));
        }
    };
    if part.kind != part_type {
        return Err(Error::Mismatch(format!("{label} type changed")));
    }
    if (part.first_lba, part.last_lba) != (expected.first_lba, expected.last_lba) {
        return Err(Error::Mismatch(format!("{label} extent changed")));
    }
    let (children, points) = holders_and_mounts(host, &part.node)?;
    if !children.is_empty() || points > 0 {
        return Err(Error::Mismatch(format!("{label} is in use")));
    }
    Ok(Some(part.clone()))
}

/// Where the Tilo root ends once staging is gone, or `None` if not adjacent.
#[must_use]
pub fn grown_end(record: &Record) -> Option<i128> {
    let (esp, root, target) = (
        &record.staging_esp,
        &record.staging_root,
        &record.target_root,
    );
    if target.last_lba.saturating_add(1) != esp.first_lba {
        return None;
    }
    if root.first_lba == esp.last_lba.saturating_add(1) {
        Some(root.last_lba)
    } else {
        Some(esp.last_lba)
    }
}

/// What a run will do: the staging partitions still present, the Tilo root,
/// and where the root grows to (`None` when it already has).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub esp: Option<Partition>,
    pub root: Option<Partition>,
    pub target: Partition,
    pub grow_to: Option<i128>,
}

/// Decides every step from the disk as it is now.
pub fn plan(host: &dyn Host, record: &Record, parts: &[Partition]) -> Result<Plan, Error> {
    let esp = staged_state(host, parts, &record.staging_esp, ESP_TYPE, "staging ESP")?;
    let root = staged_state(
        host,
        parts,
        &record.staging_root,
        STAGING_ROOT_TYPE,
        "staging root",
    )?;
    let targets: Vec<usize> = (0..parts.len())
        .filter(|i| {
            parts
                .get(*i)
                .is_some_and(|p| p.partuuid == record.target_root.partuuid)
        })
        .collect();
    let [target_index] = targets.as_slice() else {
        return Err(Error::Mismatch(
            "the Tilo root partition is not present exactly once".to_owned(),
        ));
    };
    let target = parts
        .get(*target_index)
        .cloned()
        .ok_or_else(|| Error::Crash("index".to_owned()))?;
    if target.kind != TILO_ROOT_TYPE || target.first_lba != record.target_root.first_lba {
        return Err(Error::Mismatch(
            "the Tilo root partition type or start changed".to_owned(),
        ));
    }
    let end = grown_end(record);
    let grow_to = if target.last_lba == record.target_root.last_lba {
        end
    } else if end == Some(target.last_lba) && esp.is_none() && root.is_none() {
        None
    } else {
        return Err(Error::Mismatch(
            "the Tilo root ends neither at its recorded end nor at the reclaimed end".to_owned(),
        ));
    };
    // Only the staging partitions themselves, or the Tilo root once it has
    // grown into their space, may occupy the recorded extents.
    let mut allowed = vec![
        record.staging_esp.partuuid.clone(),
        record.staging_root.partuuid.clone(),
    ];
    if grow_to.is_none() && target.last_lba != record.target_root.last_lba {
        allowed.push(target.partuuid.clone());
    }
    for extent in [&record.staging_esp, &record.staging_root] {
        for part in parts {
            if !allowed.contains(&part.partuuid) && part.overlaps(extent.first_lba, extent.last_lba)
            {
                return Err(Error::Mismatch(format!(
                    "{} now occupies the recorded staging space",
                    part.node
                )));
            }
        }
    }
    if let Some(grow_to) = grow_to {
        let first = record.target_root.last_lba.saturating_add(1);
        for (i, part) in parts.iter().enumerate() {
            if !allowed.contains(&part.partuuid)
                && i != *target_index
                && part.overlaps(first, grow_to)
            {
                return Err(Error::Mismatch(format!(
                    "{} lies inside the space the Tilo root would grow into",
                    part.node
                )));
            }
        }
    }
    Ok(Plan {
        esp,
        root,
        target,
        grow_to,
    })
}

/// Deletes the staging partitions and grows the Tilo root's GPT entry
/// (under the disk lock), then its LUKS mapping and filesystem.
///
/// # Errors
/// A mismatch before any change, or a failed step.
pub fn reclaim(host: &dyn Host, record: &Record) -> Result<(), Error> {
    let disk = find_disk(host, record)?;
    let target = {
        let _lock = host.lock(&disk)?;
        let parts = partition_table(host, &disk, record)?;
        let plan = plan(host, record, &parts)?;
        for (part, label) in [(&plan.esp, "staging ESP"), (&plan.root, "staging root")] {
            let Some(part) = part else { continue };
            let number = partition_number(&part.node)?.to_string();
            checked(
                host,
                &[
                    "sfdisk",
                    "--no-reread",
                    "--lock=no",
                    "--delete",
                    &disk,
                    &number,
                ],
            )?;
            host.run(&["partx", "-d", "--nr", &number, &disk], None)?;
            host.log(&format!("deleted the {label} ({})", part.partuuid));
        }
        let number = partition_number(&plan.target.node)?.to_string();
        if let Some(grow_to) = plan.grow_to {
            let size = grow_to
                .saturating_sub(plan.target.first_lba)
                .saturating_add(1);
            let script = format!("start={}, size={size}\n", plan.target.first_lba);
            let argv = [
                "sfdisk",
                "--no-reread",
                "--lock=no",
                "-N",
                number.as_str(),
                disk.as_str(),
            ];
            let outcome = host.run(&argv, Some(&script))?;
            if outcome.code != 0 {
                return Err(Error::Incomplete(format!(
                    "Command '{}' returned non-zero exit status {}.",
                    py::repr_list(&argv),
                    outcome.code
                )));
            }
            checked(host, &["partx", "-u", "--nr", &number, &disk])?;
            let after: Vec<Partition> = partition_table(host, &disk, record)?
                .into_iter()
                .filter(|p| p.partuuid == record.target_root.partuuid)
                .collect();
            let persisted = matches!(after.as_slice(), [grown]
                if grown.kind == TILO_ROOT_TYPE && (grown.first_lba, grown.last_lba) == (plan.target.first_lba, grow_to));
            if !persisted {
                return Err(Error::Incomplete(
                    "the grown Tilo root entry did not persist as planned".to_owned(),
                ));
            }
            host.log(&format!("grew the Tilo root partition to LBA {grow_to}"));
        } else {
            // Idempotent after a crash between the GPT write and the kernel
            // update: make sure the running kernel sees the recorded size.
            checked(host, &["partx", "-u", "--nr", &number, &disk])?;
        }
        plan.target
    };
    grow_filesystem(host, &target.node)
}

/// Grows the LUKS mapping (if any) and the filesystem of the physical root.
fn grow_filesystem(host: &dyn Host, node: &str) -> Result<(), Error> {
    let (children, _) = holders_and_mounts(host, node)?;
    let mut mappers = Vec::new();
    for child in &children {
        if get(child, "type")?.and_then(Value::as_str) == Some("crypt") {
            mappers.push(py::str_of(Some(index(child, "name")?)));
        }
    }
    if mappers.len() > 1 {
        return Err(Error::Incomplete(
            "the Tilo root has more than one encrypted mapping".to_owned(),
        ));
    }
    if let Some(mapper) = mappers.first() {
        let name = mapper.rsplit('/').next().unwrap_or(mapper);
        let resize = host.run(&["cryptsetup", "resize", "--batch-mode", name], None)?;
        if resize.code != 0 {
            // The mapping is sized to its partition at the next unlock, and
            // the existing root-growth unit then finishes the job on that boot.
            host.log(&format!(
                "could not grow the encrypted mapping now ({}); it grows at the next boot",
                resize.stderr.trim()
            ));
            return Ok(());
        }
    }
    let root = host.root_mount().display().to_string();
    let fstype = checked(host, &["findmnt", "-no", "FSTYPE", &root])?
        .stdout
        .trim()
        .to_owned();
    if fstype == "btrfs" {
        checked(host, &["btrfs", "filesystem", "resize", "max", &root])?;
    } else {
        checked(host, &["/usr/lib/systemd/systemd-growfs", &root])?;
    }
    host.log(&format!("grew the {fstype} filesystem at {root}"));
    Ok(())
}

fn remove_record(path: &Path) -> Result<(), Error> {
    std::fs::remove_file(path).map_err(|e| Error::Crash(Error::os(&e, path).to_string()))?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let fd = rustix::fs::open(
        directory,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| Error::Crash(Error::os(&e.into(), directory).to_string()))?;
    rustix::fs::fsync(&fd).map_err(|e| Error::Crash(Error::os(&e.into(), directory).to_string()))
}

fn run_main(host: &dyn Host) -> Result<i32, Error> {
    let record_path = host.record();
    if std::fs::metadata(&record_path).is_err() {
        return Ok(0);
    }
    if !user_session_exists(host)? {
        host.log("waiting for the first user session before reclaiming installer space");
        return Ok(0);
    }
    match record::load(&record_path).and_then(|record| reclaim(host, &record)) {
        Ok(()) => {}
        Err(Error::Mismatch(message)) => {
            // `refused:` and `done` are stable markers: the unit mirrors them
            // to the console, where the VM round trip reads them.
            host.log(&format!(
                "refused: left the installer partitions untouched: {message}"
            ));
            return Ok(1);
        }
        Err(Error::Incomplete(message)) => {
            // Kept for the next run, which resumes from the disk as it is.
            host.log(&format!(
                "incomplete: stopped before finishing; will resume on the next run: {message}"
            ));
            return Ok(1);
        }
        Err(crash) => return Err(crash),
    }
    remove_record(&record_path)?;
    host.log("returned the temporary installer space to Tilo");
    host.log("done");
    Ok(0)
}

/// One run; returns the exit status.
pub fn main(host: &dyn Host) -> i32 {
    match run_main(host) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tilo-staging-reclaim: unexpected failure: {error}");
            1
        }
    }
}

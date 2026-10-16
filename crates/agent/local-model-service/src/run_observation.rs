//! Content-free owned-runtime counters. These are observations, not resident-fit attestations.
use crate::ProcessHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const MAX_READ: u64 = 65_536;

/// An unavailable counter is never encoded as an observed zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observed<T> {
    Actual { value: T },
    Unavailable { reason: String },
}
impl<T> Observed<T> {
    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

/// Identity of the exact process and visible cgroup sampled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub cgroup: String,
    pub membership: String,
    pub directory_device: u64,
    pub directory_inode: u64,
}
/// Actual aggregate counters of one cgroup, which may include child processes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupSample {
    pub identity: ProcessIdentity,
    pub counters: BTreeMap<String, Observed<BTreeMap<String, u64>>>,
    pub memory_max: Observed<String>,
    pub memory_high: Observed<String>,
}
/// Wrapper and model payload have distinct process/cgroup attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSample {
    pub boot_id: String,
    pub unit_invocation_id: String,
    pub unit_control_group: String,
    pub elapsed_micros: u64,
    pub captured_at_unix_ms: u64,
    pub wrapper: CgroupSample,
    pub payload: CgroupSample,
}
/// Private, immutable checkpoint; it contains no prompts, output or environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCheckpoint {
    pub schema: u16,
    pub phase: String,
    pub model_id: String,
    pub model_sha256: String,
    pub runtime_id: Option<String>,
    pub process: ProcessHandle,
    pub sample: Observed<RuntimeSample>,
}

fn read(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "observation file unavailable")?;
    if !metadata.is_file() || metadata.len() > MAX_READ {
        return Err("observation file is not bounded and regular".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "observation file could not be opened")?;
    let mut body = String::new();
    file.take(MAX_READ + 1)
        .read_to_string(&mut body)
        .map_err(|_| "observation file is unreadable")?;
    if body.len() > usize::try_from(MAX_READ).map_err(|_| "invalid read bound")? {
        return Err("observation exceeds read bound".into());
    }
    Ok(body)
}

fn cgroup_path(membership: &str, mounts: &str) -> Result<PathBuf, String> {
    let groups: Vec<_> = membership
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect();
    let [group] = groups.as_slice() else {
        return Err("unified cgroup membership unavailable".into());
    };
    let group = Path::new(group);
    if !group.is_absolute()
        || group
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err("invalid unified cgroup path".into());
    }
    let mut candidates = Vec::new();
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        if right.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let fields: Vec<_> = left.split_whitespace().collect();
        let (Some(root), Some(mount)) = (fields.get(3), fields.get(4)) else {
            return Err("malformed cgroup mount".into());
        };
        if !Path::new(root).is_absolute()
            || Path::new(root)
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err("invalid cgroup mount root".into());
        }
        if root.contains('\\') || mount.contains('\\') {
            return Err("escaped cgroup mount unsupported".into());
        }
        if let Ok(relative) = group.strip_prefix(root) {
            let base = Path::new(mount);
            if !base.is_absolute()
                || base
                    .components()
                    .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
            {
                return Err("invalid cgroup mount path".into());
            }
            candidates.push(base.join(relative));
        }
    }
    let [path] = candidates.as_slice() else {
        return Err("visible unified cgroup mount is ambiguous or absent".into());
    };
    for ancestor in path.ancestors() {
        if !fs::symlink_metadata(ancestor)
            .map_err(|_| "cgroup directory unavailable")?
            .is_dir()
        {
            return Err("cgroup directory contains non-directory or symlink".into());
        }
    }
    Ok(path.clone())
}

fn process_identity(pid: u32) -> Result<(ProcessIdentity, PathBuf), String> {
    if pid == 0 {
        return Err("payload PID unavailable".into());
    }
    let proc = PathBuf::from(format!("/proc/{pid}"));
    let stat = read(&proc.join("stat"))?;
    let (_, tail) = stat.rsplit_once(") ").ok_or("malformed process stat")?;
    let start_ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or("process start time missing")?
        .parse()
        .map_err(|_| "process start time malformed")?;
    let membership = read(&proc.join("cgroup"))?;
    let group = cgroup_path(&membership, &read(Path::new("/proc/self/mountinfo"))?)?;
    let metadata = fs::symlink_metadata(&group).map_err(|_| "cgroup directory unavailable")?;
    Ok((
        ProcessIdentity {
            pid,
            start_ticks,
            cgroup: group.display().to_string(),
            membership: membership
                .trim()
                .strip_prefix("0::")
                .ok_or("invalid membership")?
                .into(),
            directory_device: metadata.dev(),
            directory_inode: metadata.ino(),
        },
        group,
    ))
}

pub(crate) fn verify_process(identity: &ProcessIdentity) -> Result<(), String> {
    if process_identity(identity.pid)?.0 != *identity {
        return Err("process or cgroup lifetime changed".into());
    }
    Ok(())
}

fn numbers(body: &str, singleton: bool) -> Result<BTreeMap<String, u64>, String> {
    let mut values = BTreeMap::new();
    if singleton {
        values.insert(
            "bytes".into(),
            body.trim().parse().map_err(|_| "invalid byte counter")?,
        );
    } else {
        for line in body.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            let [key, value] = fields.as_slice() else {
                return Err("malformed counter row".into());
            };
            if values.len() >= 256 || key.len() > 64 {
                return Err("counter count or key exceeds bound".into());
            }
            let number = value.parse().map_err(|_| "malformed counter value")?;
            if values.insert((*key).into(), number).is_some() {
                return Err("duplicate counter".into());
            }
        }
        if values.is_empty() {
            return Err("empty counter file".into());
        }
    }
    Ok(values)
}
fn metric(path: &Path, singleton: bool) -> Observed<BTreeMap<String, u64>> {
    let parsed = read(path)
        .and_then(|body| numbers(&body, singleton))
        .and_then(|mut values| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("counter filename missing")?;
            let keys: &[&str] = match name {
                "memory.stat" => &[
                    "anon",
                    "file",
                    "shmem",
                    "file_mapped",
                    "kernel",
                    "slab",
                    "sock",
                    "active_anon",
                    "inactive_anon",
                    "pgfault",
                    "pgmajfault",
                ],
                "memory.events" => &[
                    "low",
                    "high",
                    "max",
                    "oom",
                    "oom_kill",
                    "oom_group_kill",
                    "sock_throttled",
                ],
                "cpu.stat" => &[
                    "usage_usec",
                    "user_usec",
                    "system_usec",
                    "nr_periods",
                    "nr_throttled",
                    "throttled_usec",
                    "nr_bursts",
                    "burst_usec",
                ],
                _ => &["bytes"],
            };
            values.retain(|key, _| keys.contains(&key.as_str()));
            if values.is_empty() {
                return Err("selected counters unavailable".into());
            }
            Ok(values)
        });
    match parsed {
        Ok(value) => Observed::Actual { value },
        Err(reason) => Observed::Unavailable { reason },
    }
}
fn limit(path: &Path) -> Observed<String> {
    match read(path).and_then(|body| {
        let value = body.trim();
        if value == "max" || value.parse::<u64>().is_ok() {
            Ok(value.into())
        } else {
            Err("malformed memory limit".into())
        }
    }) {
        Ok(value) => Observed::Actual { value },
        Err(reason) => Observed::Unavailable { reason },
    }
}
/// Observe one process's aggregate cgroup and fence process/cgroup lifetime.
/// # Errors
/// Refuses missing identities, inaccessible mount roots and PID/cgroup reuse.
pub(crate) fn sample_process(pid: u32) -> Result<CgroupSample, String> {
    let (identity, group) = process_identity(pid)?;
    let mut counters = BTreeMap::new();
    for name in ["memory.current", "memory.peak", "memory.swap.current"] {
        counters.insert(name.into(), metric(&group.join(name), true));
    }
    for name in ["memory.stat", "memory.events", "cpu.stat"] {
        counters.insert(name.into(), metric(&group.join(name), false));
    }
    // io.stat uses per-device key=value records, unlike two-column memory counters.
    counters.insert(
        "io.stat".into(),
        match read(&group.join("io.stat")).and_then(|body| io_numbers(&body)) {
            Ok(value) => Observed::Actual { value },
            Err(reason) => Observed::Unavailable { reason },
        },
    );
    let sample = CgroupSample {
        identity: identity.clone(),
        counters,
        memory_max: limit(&group.join("memory.max")),
        memory_high: limit(&group.join("memory.high")),
    };
    if process_identity(pid)?.0 != identity {
        return Err("process or cgroup lifetime changed".into());
    }
    Ok(sample)
}
fn io_numbers(body: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut output = BTreeMap::new();
    for line in body.lines() {
        let mut fields = line.split_whitespace();
        let device = fields.next().ok_or("missing I/O device")?;
        let (major, minor) = device.split_once(':').ok_or("invalid I/O device")?;
        major.parse::<u32>().map_err(|_| "invalid I/O device")?;
        minor.parse::<u32>().map_err(|_| "invalid I/O device")?;
        for field in fields {
            if output.len() >= 128 {
                return Err("I/O counter count exceeds bound".into());
            }
            let (key, value) = field.split_once('=').ok_or("invalid I/O counter")?;
            if !matches!(
                key,
                "rbytes" | "wbytes" | "rios" | "wios" | "dbytes" | "dios"
            ) {
                return Err("I/O counter key unsupported".into());
            }
            if output
                .insert(
                    format!("{device}/{key}"),
                    value.parse().map_err(|_| "invalid I/O value")?,
                )
                .is_some()
            {
                return Err("duplicate I/O counter".into());
            }
        }
    }
    // An empty io.stat is actual no exposed device counters, not zero-byte I/O.
    Ok(output)
}

fn directory(root: &Path) -> Result<PathBuf, String> {
    let directory = root.join("run-observations");
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let m = fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
    if !m.is_dir() || !tilo_files::native::private::private_to_me(&m) {
        return Err("run observation directory is not private and owned".into());
    }
    Ok(directory)
}
fn validate_checkpoint(checkpoint: &RunCheckpoint) -> Result<String, String> {
    let owned = checkpoint
        .process
        .container
        .as_ref()
        .ok_or("missing checkpoint custody")?;
    owned.validate()?;
    if checkpoint.schema != 1
        || checkpoint.process.id != owned.unit()
        || !matches!(
            checkpoint.phase.as_str(),
            "ready" | "before_stop" | "failed_start"
        )
        || checkpoint.model_sha256.len() != 64
        || !checkpoint
            .model_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || owned.model_id_sha256 != hex::encode(Sha256::digest(checkpoint.model_id.as_bytes()))
    {
        return Err("invalid checkpoint model, identity or phase".into());
    }
    Ok(format!("{}-{}.json", owned.run_id, checkpoint.phase))
}

/// Retain a new exclusive private checkpoint, never rewriting an earlier run.
/// # Errors
/// Refuses malformed custody, unsafe storage and duplicate checkpoints.
pub(crate) fn retain(root: &Path, checkpoint: &RunCheckpoint) -> Result<(), String> {
    let name = validate_checkpoint(checkpoint)?;
    let bytes = serde_json::to_vec(checkpoint).map_err(|e| e.to_string())?;
    if bytes.len() > usize::try_from(MAX_READ).map_err(|_| "invalid checkpoint bound")? {
        return Err("checkpoint exceeds bound".into());
    }
    let directory = directory(root)?;
    let path = directory.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
/// Read bounded retained observations; errors are explicit rather than empty success.
#[must_use]
pub fn retained(root: &Path) -> Observed<Vec<RunCheckpoint>> {
    match read_retained(root) {
        Ok(value) => Observed::Actual { value },
        Err(reason) => Observed::Unavailable { reason },
    }
}
fn read_retained(root: &Path) -> Result<Vec<RunCheckpoint>, String> {
    match fs::symlink_metadata(root.join("run-observations")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("retained observation directory unavailable".into()),
        Ok(_) => {}
    }
    let directory = directory(root)?;
    let mut records = Vec::new();
    let mut total_bytes = 0_u64;
    for item in fs::read_dir(directory).map_err(|e| e.to_string())? {
        if records.len() >= 256 {
            return Err("retained observation count exceeds bound".into());
        }
        let path = item.map_err(|e| e.to_string())?.path();
        let m = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        total_bytes = total_bytes
            .checked_add(m.len())
            .ok_or("retained observation size overflow")?;
        if total_bytes > 262_144 {
            return Err("retained observation bytes exceed bound".into());
        }
        if !tilo_files::native::private::private_to_me(&m) {
            return Err("observation file is not private and owned".into());
        }
        let record: RunCheckpoint =
            serde_json::from_str(&read(&path)?).map_err(|_| "invalid retained observation")?;
        if path.file_name().and_then(|name| name.to_str())
            != Some(validate_checkpoint(&record)?.as_str())
        {
            return Err("retained checkpoint filename differs from custody".into());
        }
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests;

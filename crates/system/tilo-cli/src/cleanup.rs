//! Disk cleanup by policy: what this machine can download or regenerate again, removed once
//! nothing uses it.
//!
//! Two scopes, because the caches have two owners:
//!
//! * **System** (root, `tilo-cache-gc.timer`): Flatpak runtimes no installed app uses, the
//!   copy of the last developer image upgrade in `/var/lib/tilo-vm/image`, and the OS image
//!   data no deployment uses any more (`tilo-os-image-gc.service`, also after every update
//!   and while the disk is low on space).
//! * **User** (the person's own `tilo-cache-gc.timer`): the test fixture caches
//!   `tools/test-env.sh` keeps in `$XDG_CACHE_HOME/tilo/test-env`, keyed by the revision of
//!   what generated them, so every change to a generator leaves the previous set behind.
//!
//! The policy is `/etc/tilo/cache-gc.toml`; a person's `~/.config/tilo/cache-gc.toml`
//! replaces it for their own run, like `desktop.toml`. Each run writes a record
//! (`/var/lib/tilo-cache-gc/record.json`, `$XDG_STATE_HOME/tilo/cache-gc.json`) that
//! `tilo os status` reads to say what was freed, what was kept and when that can go.
//!
//! ## What counts as in use
//!
//! * A Flatpak runtime: whatever Flatpak itself counts as used. This module never names a
//!   runtime to remove; it asks `flatpak uninstall --unused`, which keeps every runtime and
//!   extension an installed app needs and every pinned runtime, and reports the refs that
//!   are gone afterwards.
//! * The VM image copy: anything holding `tilo-vm`'s `image.lock` (an upgrade copying into
//!   it) or `vm.lock` (a switch reading it). The cleanup takes both before it measures and
//!   holds them while it removes, so an upgrade starting meanwhile waits or refuses.
//! * A test cache: anything holding the cache's `.lock`, which every generator takes while
//!   it writes and publishes. Held the same way while the entry goes.
//! * OS image data: every deployment with a boot entry (the running one, the staged one and
//!   the rollback), as bootc decides: the collection is `bootc internals composefs-gc
//!   --prune-repo`, which removes the boot files, deployment state and repository objects
//!   of images no boot entry names, such as an update that was cancelled half-way or a
//!   generation an update superseded. It runs only while it holds the update lock every OS
//!   update holds from download to activation, and the ESP's lock, which
//!   `tilo-deployment` holds while it renames boot entries; an update running meanwhile
//!   keeps it for the collection that runs after that update.
//!
//! Anything not in use goes once it has not been used for the policy's number of days, read
//! from access and modification times (relatime still records a read once a day), or at once
//! while the disk has less free space than the policy's floor.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use advisory_lock::{FileLock, LockMode, TryLockError};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::os::human_size;

/// The image's policy.
pub const SYSTEM_POLICY: &str = "/etc/tilo/cache-gc.toml";
/// The system run's record, readable by everyone so `tilo os status` needs no root.
pub const SYSTEM_RECORD: &str = "/var/lib/tilo-cache-gc/record.json";
/// `tilo-vmd`'s state root, which holds the image copy and the locks guarding it.
pub const VM_ROOT: &str = "/var/lib/tilo-vm";
/// Flatpak's system installation.
pub const FLATPAK_ROOT: &str = "/var/lib/flatpak";
/// The OS image collection's record, beside [`SYSTEM_RECORD`]: it runs in its own unit.
pub const OS_IMAGE_RECORD: &str = "/var/lib/tilo-cache-gc/os-images.json";
/// The lock `tilo-verified-update` holds from an update's download to its activation.
pub const UPDATE_LOCK: &str = "/run/tilo-verified-update.lock";
/// The physical root, which holds the composefs repository and the deployments.
pub const SYSROOT: &str = "/sysroot";

const DAY: u64 = 24 * 60 * 60;

/// What may be removed, and after how long. Every table and key is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub flatpak: FlatpakPolicy,
    pub vm_image: AgePolicy,
    pub test_caches: AgePolicy,
    pub os_images: OsImagePolicy,
    pub space: SpacePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsImagePolicy {
    /// Remove the OS image data no deployment uses.
    pub remove: bool,
}

impl Default for OsImagePolicy {
    fn default() -> Self {
        Self { remove: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlatpakPolicy {
    /// Remove runtimes no installed app uses.
    pub remove_unused_runtimes: bool,
}

impl Default for FlatpakPolicy {
    fn default() -> Self {
        Self {
            remove_unused_runtimes: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgePolicy {
    /// Whether this kind is cleaned at all.
    pub remove: bool,
    /// Days without use after which an entry goes.
    pub unused_days: u64,
}

impl Default for AgePolicy {
    fn default() -> Self {
        Self {
            remove: true,
            unused_days: 14,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpacePolicy {
    /// Below this much free space, anything not in use goes regardless of age.
    pub low_free_gb: u64,
}

impl Default for SpacePolicy {
    fn default() -> Self {
        Self { low_free_gb: 5 }
    }
}

impl Policy {
    /// Parse a policy file's text.
    ///
    /// # Errors
    /// A malformed policy is an error rather than the defaults: an owner who turned a kind
    /// off and mistyped the file must not get it cleaned.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The first of `paths` that exists, parsed; the defaults when none does.
    ///
    /// # Errors
    /// When the file exists but cannot be read or parsed.
    pub fn load(paths: &[PathBuf]) -> Result<Self> {
        for path in paths {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    return Self::parse(&text)
                        .with_context(|| format!("reading the cleanup policy {}", path.display()));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("reading the cleanup policy {}", path.display()));
                }
            }
        }
        Ok(Self::default())
    }
}

/// What a recorded entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    FlatpakRuntime,
    VmImage,
    TestCache,
    OsImage,
}

/// Why an entry stayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeptBecause {
    /// Something holds its lock.
    InUse,
    /// Used within the policy's number of days.
    Recent,
    /// The policy does not clean this kind.
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub kind: Kind,
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub kind: Kind,
    pub name: String,
    pub bytes: u64,
    pub because: KeptBecause,
    /// When a [`KeptBecause::Recent`] entry becomes old enough, in seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub kind: Kind,
    pub name: String,
    pub error: String,
}

/// One run's outcome, as `tilo os status` reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When the run finished, in seconds since the epoch.
    pub finished: u64,
    #[serde(default)]
    pub removed: Vec<Removed>,
    #[serde(default)]
    pub kept: Vec<Kept>,
    #[serde(default)]
    pub failed: Vec<Failed>,
}

impl Record {
    pub fn freed(&self) -> u64 {
        self.removed
            .iter()
            .fold(0, |sum, item| sum.saturating_add(item.bytes))
    }
}

/// What the clock and the disk say, so a decision can be tested without either.
#[derive(Debug, Clone, Copy)]
pub struct Conditions {
    pub now: SystemTime,
    /// Free bytes on the filesystem being cleaned; `None` when unknown.
    pub free: Option<u64>,
}

impl Conditions {
    /// Whether the disk has less free space than the policy's floor.
    pub fn low_on_space(&self, policy: &Policy) -> bool {
        let floor = policy.space.low_free_gb.saturating_mul(1_000_000_000);
        self.free.is_some_and(|free| free < floor)
    }
}

/// Whether one entry goes: `Ok(())` to remove it, `Err` with why it stays.
pub fn decide(
    last_used: SystemTime,
    policy: &AgePolicy,
    space: &Policy,
    conditions: Conditions,
) -> std::result::Result<(), (KeptBecause, Option<u64>)> {
    if !policy.remove {
        return Err((KeptBecause::Disabled, None));
    }
    if conditions.low_on_space(space) {
        return Ok(());
    }
    let limit = Duration::from_secs(policy.unused_days.saturating_mul(DAY));
    let eligible = last_used.checked_add(limit).unwrap_or(last_used);
    if eligible <= conditions.now {
        Ok(())
    } else {
        Err((KeptBecause::Recent, Some(epoch_secs(eligible))))
    }
}

fn epoch_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn from_epoch(secs: i64) -> SystemTime {
    u64::try_from(secs).map_or(UNIX_EPOCH, |secs| {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(secs))
            .unwrap_or(UNIX_EPOCH)
    })
}

/// Disk usage and the latest use under `path`, without following links. Hard-linked files
/// count once.
///
/// A file was used when it was last read or written. A directory only counts when its
/// entries changed: reading it (as this walk does) moves its access time, so its own
/// access time would make every directory look used by the previous run.
fn measure(path: &Path) -> (u64, SystemTime) {
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    let mut latest = UNIX_EPOCH;
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&next) else {
            continue;
        };
        latest = latest.max(from_epoch(meta.mtime()));
        if !meta.is_dir() {
            latest = latest.max(from_epoch(meta.atime()));
        }
        if seen.insert((meta.dev(), meta.ino())) {
            bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
        }
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&next)
        {
            pending.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    (bytes, latest)
}

/// A lock that must be held to touch something. `Ok(None)` when another process holds it.
fn try_hold(lock: &Path) -> Result<Option<FileLock>> {
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match FileLock::try_open(lock, LockMode::Exclusive) {
        Ok(held) => Ok(Some(held)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", lock.display()))
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Settle one entry whose locks are held: remove it or record why it stays.
fn settle(
    record: &mut Record,
    kind: Kind,
    name: String,
    path: &Path,
    age: &AgePolicy,
    policy: &Policy,
    conditions: Conditions,
) {
    let (bytes, last_used) = measure(path);
    match decide(last_used, age, policy, conditions) {
        Ok(()) => match remove_path(path) {
            Ok(()) => record.removed.push(Removed { kind, name, bytes }),
            Err(error) => record.failed.push(Failed {
                kind,
                name,
                error: error.to_string(),
            }),
        },
        Err((because, eligible)) => record.kept.push(Kept {
            kind,
            name,
            bytes,
            because,
            eligible,
        }),
    }
}

/// The developer image copy `tilo-vm vm upgrade --from local` mirrors into the VM.
///
/// # Errors
/// When the locks cannot be created or taken for a reason other than contention.
pub fn clean_vm_image(
    record: &mut Record,
    vm_root: &Path,
    policy: &Policy,
    conditions: Conditions,
) -> Result<()> {
    let layout = vm_root.join("image");
    if std::fs::symlink_metadata(&layout).is_err() {
        return Ok(());
    }
    let name = "the copy of the last developer image upgrade".to_owned();
    let locks = vm_root.join("locks");
    let image = try_hold(&locks.join("image.lock"))?;
    let vm = try_hold(&locks.join("vm.lock"))?;
    if image.is_none() || vm.is_none() {
        let (bytes, _) = measure(&layout);
        record.kept.push(Kept {
            kind: Kind::VmImage,
            name,
            bytes,
            because: KeptBecause::InUse,
            eligible: None,
        });
        return Ok(());
    }
    settle(
        record,
        Kind::VmImage,
        name,
        &layout,
        &policy.vm_image,
        policy,
        conditions,
    );
    drop((image, vm));
    Ok(())
}

/// The test fixture caches below `root`. Each directory directly under it is one cache,
/// guarded by its `.lock`; its entries go one by one. A file directly under `root` is
/// guarded by `root/.lock`.
///
/// # Errors
/// When a lock cannot be created or taken for a reason other than contention.
pub fn clean_test_caches(
    record: &mut Record,
    root: &Path,
    policy: &Policy,
    conditions: Conditions,
) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    let mut caches = Vec::new();
    let mut loose = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name() == ".lock" {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            caches.push(entry.path());
        } else {
            loose.push(entry.path());
        }
    }
    caches.sort();
    loose.sort();
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for cache in caches {
        let mut members: Vec<PathBuf> = std::fs::read_dir(&cache)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_name() != ".lock")
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        members.sort();
        groups.push((cache.join(".lock"), members));
    }
    groups.push((root.join(".lock"), loose));
    for (lock, members) in groups {
        if members.is_empty() {
            continue;
        }
        let held = try_hold(&lock)?;
        for member in &members {
            let name = member
                .strip_prefix(root)
                .unwrap_or(member)
                .display()
                .to_string();
            if held.is_some() {
                settle(
                    record,
                    Kind::TestCache,
                    name,
                    member,
                    &policy.test_caches,
                    policy,
                    conditions,
                );
            } else {
                let (bytes, _) = measure(member);
                record.kept.push(Kept {
                    kind: Kind::TestCache,
                    name,
                    bytes,
                    because: KeptBecause::InUse,
                    eligible: None,
                });
            }
        }
        drop(held);
    }
    Ok(())
}

/// The system Flatpak installation, through its own command.
pub struct Flatpak {
    pub program: PathBuf,
    pub root: PathBuf,
}

impl Flatpak {
    fn runtimes(&self) -> Result<Vec<String>> {
        let output = Command::new(&self.program)
            .args(["list", "--system", "--runtime", "--columns=ref"])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.program.display()))?;
        if !output.status.success() {
            anyhow::bail!("{}", last_line(&output.stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| line.split('/').count() == 3)
            .map(str::to_owned)
            .collect())
    }

    /// Where a runtime ref (`id/arch/branch`) is deployed.
    fn deployed(&self, reference: &str) -> PathBuf {
        let mut path = self.root.join("runtime");
        path.extend(reference.split('/'));
        path
    }

    fn uninstall_unused(&self) -> Result<()> {
        let output = Command::new(&self.program)
            .args([
                "uninstall",
                "--system",
                "--unused",
                "--noninteractive",
                "-y",
            ])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.program.display()))?;
        if output.status.success() {
            Ok(())
        } else {
            anyhow::bail!("{}", last_line(&output.stderr))
        }
    }
}

fn last_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("failed without saying why")
        .trim()
        .to_owned()
}

/// Runtimes no installed app uses, as Flatpak decides; records the refs that are gone after.
pub fn clean_flatpak(record: &mut Record, flatpak: &Flatpak, policy: &Policy) {
    if !policy.flatpak.remove_unused_runtimes || !flatpak.root.exists() {
        return;
    }
    let failed = |record: &mut Record, error: &anyhow::Error| {
        record.failed.push(Failed {
            kind: Kind::FlatpakRuntime,
            name: "unused Flatpak runtimes".into(),
            error: format!("{error:#}"),
        });
    };
    let before = match flatpak.runtimes() {
        Ok(refs) => refs,
        Err(error) => return failed(record, &error),
    };
    // Measured before, since the deployment is gone afterwards.
    let sizes: Vec<(String, u64)> = before
        .iter()
        .map(|reference| (reference.clone(), measure(&flatpak.deployed(reference)).0))
        .collect();
    let outcome = flatpak.uninstall_unused();
    // Whatever went is reported, even when Flatpak stopped part-way.
    let after: BTreeSet<String> = flatpak.runtimes().map_or_else(
        |_| before.iter().cloned().collect(),
        |refs| refs.into_iter().collect(),
    );
    for (reference, bytes) in sizes {
        if !after.contains(&reference) {
            record.removed.push(Removed {
                kind: Kind::FlatpakRuntime,
                name: reference,
                bytes,
            });
        }
    }
    if let Err(error) = outcome {
        failed(record, &error);
    }
}

/// bootc, and what its collection must not run alongside.
pub struct Bootc {
    pub program: PathBuf,
    /// The physical root; `<sysroot>/composefs` is the repository. A machine without one
    /// did not boot bootc's composefs backend and has nothing to collect.
    pub sysroot: PathBuf,
    /// The lock every OS update holds from its download to its activation.
    pub update_lock: PathBuf,
    /// The ESP, when this machine boots through systemd-boot entries: `tilo-deployment`
    /// holds its directory's lock while it renames them.
    pub esp: Option<PathBuf>,
    /// Where the ESP is mounted when it may be mounted read-only; bootc needs it writable
    /// to remove the boot files of the images it collects, so it is made writable for the
    /// one command and read-only again after.
    pub boot_mount: Option<PathBuf>,
}

/// What the cleanup calls the data it collects.
pub const OS_IMAGE_NAME: &str = "OS image data no deployment uses";

/// Runs bootc's collection with the ESP writable for the one command when it was mounted
/// read-only, read-only again after. `$0` is bootc, `$1` the ESP's mount point.
const COLLECT_SCRIPT: &str = "m=$1; ro=; if findmnt -n -o FSTYPE,OPTIONS \"$m\" 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw \"$m\" || exit 1; fi; \"$0\" internals composefs-gc --prune-repo; rc=$?; [ -z \"$ro\" ] || mount -o remount,ro \"$m\"; exit $rc";

/// The update lock, created as `tilo-verified-update` requires it (owner-only, one link,
/// never through a symbolic link). `Ok(None)` while an update holds it.
fn try_hold_update_lock(path: &Path) -> Result<Option<FileLock>> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    match FileLock::try_exclusive(file) {
        Ok(held) => Ok(Some(held)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// The lock on a directory itself, as `flock DIR` takes it. `Ok(None)` while held.
fn try_hold_directory(path: &Path) -> Result<Option<FileLock>> {
    let directory =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    match FileLock::try_exclusive(directory) {
        Ok(held) => Ok(Some(held)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// The OS image data no deployment uses, as bootc decides: see the module's account of
/// what counts as in use. Records the space it freed.
pub fn clean_os_images(record: &mut Record, bootc: &Bootc, policy: &Policy) {
    if !bootc.sysroot.join("composefs").is_dir() {
        return;
    }
    let kept = |record: &mut Record, because| {
        record.kept.push(Kept {
            kind: Kind::OsImage,
            name: OS_IMAGE_NAME.into(),
            bytes: 0,
            because,
            eligible: None,
        });
    };
    let failed = |record: &mut Record, error: String| {
        record.failed.push(Failed {
            kind: Kind::OsImage,
            name: OS_IMAGE_NAME.into(),
            error,
        });
    };
    if !policy.os_images.remove {
        return kept(record, KeptBecause::Disabled);
    }
    let update = match try_hold_update_lock(&bootc.update_lock) {
        Ok(Some(held)) => held,
        Ok(None) => return kept(record, KeptBecause::InUse),
        Err(error) => return failed(record, format!("{error:#}")),
    };
    let esp = match bootc.esp.as_deref().map(try_hold_directory).transpose() {
        Ok(Some(None)) => return kept(record, KeptBecause::InUse),
        Ok(held) => held.flatten(),
        Err(error) => return failed(record, format!("{error:#}")),
    };
    let before = free_space(&bootc.sysroot);
    let mut command = if let Some(mount) = &bootc.boot_mount {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(COLLECT_SCRIPT)
            .arg(&bootc.program)
            .arg(mount);
        command
    } else {
        let mut command = Command::new(&bootc.program);
        command.args(["internals", "composefs-gc", "--prune-repo"]);
        command
    };
    let output = command.env("LC_ALL", "C").stdin(Stdio::null()).output();
    let after = free_space(&bootc.sysroot);
    drop(esp);
    drop(update);
    match output {
        Ok(output) if output.status.success() => {
            let freed = after
                .zip(before)
                .map_or(0, |(after, before)| after.saturating_sub(before));
            if freed > 0 {
                record.removed.push(Removed {
                    kind: Kind::OsImage,
                    name: OS_IMAGE_NAME.into(),
                    bytes: freed,
                });
            }
        }
        Ok(output) => failed(record, last_line(&output.stderr)),
        Err(error) => failed(
            record,
            format!("running {}: {error}", bootc.program.display()),
        ),
    }
}

/// Free bytes on the filesystem holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Write `record` to `path` atomically, readable by everyone.
///
/// # Errors
/// When the directory cannot be created or the file written.
pub fn write_record(path: &Path, record: &Record) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staging = parent.join(format!(
        ".{}.{}",
        path.file_name()
            .map_or_else(|| "record".into(), |name| name.to_string_lossy()),
        std::process::id()
    ));
    let mut file = std::fs::File::create(&staging)
        .with_context(|| format!("writing {}", staging.display()))?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&staging, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The record at `path`, if a run has written one.
pub fn read_record(path: &Path) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Where the person's run keeps its record.
pub fn user_record() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|dir| dir.join("tilo").join("cache-gc.json"))
}

/// The policy files the person's run reads, theirs first.
pub fn user_policy_paths() -> Vec<PathBuf> {
    tilo_paths::config_dir()
        .map(|dir| dir.join("tilo").join("cache-gc.toml"))
        .into_iter()
        .chain([PathBuf::from(SYSTEM_POLICY)])
        .collect()
}

/// The test fixture caches `tools/test-env.sh` uses.
pub fn test_cache_root() -> Option<PathBuf> {
    std::env::var_os("TILO_TEST_CACHE_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| tilo_paths::cache_dir().map(|dir| dir.join("tilo").join("test-env")))
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => plural(secs / 60, "minute", " ago"),
        3600..DAY => plural(secs / 3600, "hour", " ago"),
        _ if secs < 2 * DAY => "yesterday".into(),
        _ => plural(secs / DAY, "day", " ago"),
    }
}

fn within(secs: u64) -> String {
    match secs {
        0..3600 => "within the hour".into(),
        3600..DAY => format!("in {}", plural(secs / 3600, "hour", "")),
        _ => format!("in {}", plural(secs.div_ceil(DAY), "day", "")),
    }
}

fn plural(count: u64, unit: &str, suffix: &str) -> String {
    if count == 1 {
        format!("1 {unit}{suffix}")
    } else {
        format!("{count} {unit}s{suffix}")
    }
}

/// A Flatpak ref as people name it: `org.gnome.Platform/aarch64/48` → `org.gnome.Platform 48`.
fn runtime_name(reference: &str) -> String {
    let mut parts = reference.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(id), Some(_arch), Some(branch)) => format!("{id} {branch}"),
        _ => reference.to_owned(),
    }
}

fn describe(kind: Kind, name: &str) -> String {
    match kind {
        Kind::FlatpakRuntime if name.contains('/') => {
            format!("Flatpak runtime {}", runtime_name(name))
        }
        Kind::FlatpakRuntime | Kind::VmImage | Kind::OsImage => name.to_owned(),
        Kind::TestCache => format!("test cache {name}"),
    }
}

/// `tilo os status`'s account of one scope's last run, in plain language.
pub fn render(title: &str, record: Option<&Record>, now: SystemTime) -> String {
    let Some(record) = record else {
        return format!("{title}: has not run yet; it runs daily.\n");
    };
    let since = epoch_secs(now).saturating_sub(record.finished);
    let mut lines = vec![format!("{title}, last run {}:", ago(since))];
    if record.removed.is_empty() {
        lines.push("  Nothing needed removing.".into());
    } else {
        let names: Vec<String> = record
            .removed
            .iter()
            .map(|item| describe(item.kind, &item.name))
            .collect();
        lines.push(format!(
            "  Freed {}: removed {}.",
            human_size(record.freed()),
            names.join(", ")
        ));
    }
    lines.extend(record.kept.iter().map(|kept| {
        let what = format!(
            "{} ({})",
            describe(kept.kind, &kept.name),
            human_size(kept.bytes)
        );
        match (kept.because, kept.eligible) {
            (KeptBecause::Recent, Some(eligible)) => format!(
                "  Kept {what}: used recently; it can go {}.",
                within(eligible.saturating_sub(epoch_secs(now)))
            ),
            (KeptBecause::Recent, None) => format!("  Kept {what}: used recently."),
            (KeptBecause::InUse, _) => format!("  Kept {what}: it was in use."),
            (KeptBecause::Disabled, _) => format!("  Kept {what}: the cleanup policy keeps it."),
        }
    }));
    lines.extend(record.failed.iter().map(|failed| {
        format!(
            "  Could not remove {}: {}",
            describe(failed.kind, &failed.name),
            failed.error
        )
    }));
    lines.push(String::new());
    lines.join("\n")
}

/// What one root run cleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Flatpak runtimes and the VM image copy, recorded in [`SYSTEM_RECORD`]
    /// (`tilo-cache-gc.service`, sandboxed to just those).
    Caches,
    /// The OS image data no deployment uses, recorded in [`OS_IMAGE_RECORD`]
    /// (`tilo-os-image-gc.service`, which may write the physical root and the ESP).
    OsImages,
}

/// bootc as this machine runs it.
fn system_bootc() -> Bootc {
    let esp = ["/boot", "/boot/efi"]
        .into_iter()
        .map(PathBuf::from)
        .find(|esp| esp.join("loader/entries").is_dir());
    Bootc {
        program: PathBuf::from(crate::os::bootc_program()),
        sysroot: PathBuf::from(SYSROOT),
        update_lock: PathBuf::from(UPDATE_LOCK),
        esp,
        boot_mount: Some(PathBuf::from("/boot")),
    }
}

/// A root run of `scope`, recorded where `tilo os status` reads it.
///
/// # Errors
/// When the policy is unreadable or the record cannot be written.
pub fn run_system(scope: Scope) -> Result<Record> {
    let policy = Policy::load(&[PathBuf::from(SYSTEM_POLICY)])?;
    let mut record = Record::default();
    let path = match scope {
        Scope::Caches => {
            let conditions = Conditions {
                now: SystemTime::now(),
                free: free_space(Path::new("/var")),
            };
            let flatpak = Flatpak {
                program: PathBuf::from("flatpak"),
                root: PathBuf::from(FLATPAK_ROOT),
            };
            clean_flatpak(&mut record, &flatpak, &policy);
            if let Err(error) = clean_vm_image(&mut record, Path::new(VM_ROOT), &policy, conditions)
            {
                record.failed.push(Failed {
                    kind: Kind::VmImage,
                    name: "the copy of the last developer image upgrade".into(),
                    error: format!("{error:#}"),
                });
            }
            SYSTEM_RECORD
        }
        Scope::OsImages => {
            clean_os_images(&mut record, &system_bootc(), &policy);
            OS_IMAGE_RECORD
        }
    };
    record.finished = epoch_secs(SystemTime::now());
    write_record(Path::new(path), &record)?;
    Ok(record)
}

/// Whether `/var` or the physical root has less free space than the policy's floor: the
/// condition under which `tilo-cache-gc-pressure.timer` starts the cleanups at once rather
/// than at their daily run.
///
/// # Errors
/// When the policy is unreadable.
pub fn system_low_on_space() -> Result<bool> {
    let policy = Policy::load(&[PathBuf::from(SYSTEM_POLICY)])?;
    Ok(["/var", SYSROOT].into_iter().any(|path| {
        Conditions {
            now: SystemTime::now(),
            free: free_space(Path::new(path)),
        }
        .low_on_space(&policy)
    }))
}

/// The person's run: their test caches, recorded in [`user_record`].
///
/// # Errors
/// When the policy is unreadable or the record cannot be written.
pub fn run_user() -> Result<Record> {
    let policy = Policy::load(&user_policy_paths())?;
    let mut record = Record::default();
    // No test caches, no record: `tilo os status` then says nothing about them.
    let Some(root) = test_cache_root().filter(|root| root.is_dir()) else {
        return Ok(record);
    };
    let conditions = Conditions {
        now: SystemTime::now(),
        free: free_space(&root),
    };
    clean_test_caches(&mut record, &root, &policy, conditions)?;
    record.finished = epoch_secs(SystemTime::now());
    if let Some(path) = user_record() {
        write_record(&path, &record)?;
    }
    Ok(record)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test clocks are fixed far from overflow"
)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::PermissionsExt;

    const NOW: u64 = 1_800_000_000;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn conditions(free: Option<u64>) -> Conditions {
        Conditions { now: at(NOW), free }
    }

    /// A file of `bytes` last read and written `days` ago.
    fn file(path: &Path, bytes: usize, days: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
        age(path, days);
    }

    fn age(path: &Path, days: u64) {
        let when = at(NOW - days * DAY);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(when).set_modified(when))
            .unwrap();
    }

    /// Directories take the age of their newest file, so age them too.
    fn age_dir(path: &Path, days: u64) {
        let when = at(NOW - days * DAY);
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(when).set_modified(when))
            .unwrap();
    }

    #[test]
    fn the_policy_reads_from_the_owner_file_and_refuses_a_malformed_one() {
        assert_eq!(Policy::parse("").unwrap(), Policy::default());
        let policy = Policy::parse(
            "[vm_image]\nunused_days = 3\n[test_caches]\nremove = false\n[space]\nlow_free_gb = 10\n",
        )
        .unwrap();
        assert_eq!(policy.vm_image.unused_days, 3);
        assert!(policy.vm_image.remove);
        assert!(!policy.test_caches.remove);
        assert!(policy.flatpak.remove_unused_runtimes);
        assert_eq!(policy.space.low_free_gb, 10);
        // A typo must not silently fall back to cleaning everything.
        assert!(Policy::parse("[test_caches]\nremvoe = false\n").is_err());

        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("mine.toml");
        let image = dir.path().join("image.toml");
        std::fs::write(&image, "[vm_image]\nunused_days = 1\n").unwrap();
        assert_eq!(
            Policy::load(&[mine.clone(), image.clone()])
                .unwrap()
                .vm_image
                .unused_days,
            1
        );
        std::fs::write(&mine, "[vm_image]\nunused_days = 30\n").unwrap();
        assert_eq!(
            Policy::load(&[mine, image]).unwrap().vm_image.unused_days,
            30
        );
    }

    #[test]
    fn age_and_free_space_decide_what_goes() {
        let policy = Policy::default();
        let age = &policy.test_caches;
        let plenty = conditions(Some(100_000_000_000));
        assert_eq!(decide(at(NOW - 15 * DAY), age, &policy, plenty), Ok(()));
        assert_eq!(
            decide(at(NOW - 10 * DAY), age, &policy, plenty),
            Err((KeptBecause::Recent, Some(NOW + 4 * DAY)))
        );
        // Low on space: age no longer protects it.
        let low = conditions(Some(1_000_000_000));
        assert_eq!(decide(at(NOW - DAY), age, &policy, low), Ok(()));
        let off = AgePolicy {
            remove: false,
            unused_days: 0,
        };
        assert_eq!(
            decide(at(0), &off, &policy, low),
            Err((KeptBecause::Disabled, None))
        );
    }

    #[test]
    fn a_stale_vm_image_copy_goes_and_one_in_use_or_recent_stays() {
        let policy = Policy::default();
        let plenty = conditions(Some(100_000_000_000));

        // Stale: unused for 20 days.
        let root = tempfile::tempdir().unwrap();
        let layout = root.path().join("image");
        file(&layout.join("blobs/sha256/aa"), 8192, 20);
        file(&layout.join("index.json"), 100, 20);
        age_dir(&layout.join("blobs/sha256"), 20);
        age_dir(&layout.join("blobs"), 20);
        age_dir(&layout, 20);
        // A compressing filesystem may store the blob in far fewer blocks
        // than its length, and counts them only once written out.
        rustix::fs::syncfs(File::open(root.path()).unwrap()).unwrap();
        let mut record = Record::default();
        clean_vm_image(&mut record, root.path(), &policy, plenty).unwrap();
        assert!(!layout.exists(), "{record:?}");
        assert_eq!(record.removed.len(), 1);
        assert_eq!(record.removed[0].kind, Kind::VmImage);
        assert!(record.removed[0].bytes > 0, "{record:?}");

        // In use: an upgrade holds image.lock while it copies.
        file(&layout.join("index.json"), 100, 20);
        age_dir(&layout, 20);
        let upgrade =
            FileLock::open(&root.path().join("locks/image.lock"), LockMode::Exclusive).unwrap();
        let mut record = Record::default();
        clean_vm_image(&mut record, root.path(), &policy, conditions(Some(0))).unwrap();
        assert!(layout.exists());
        assert_eq!(record.kept[0].because, KeptBecause::InUse);
        drop(upgrade);

        // Recent: read by an upgrade three days ago.
        age(&layout.join("index.json"), 3);
        let mut record = Record::default();
        clean_vm_image(&mut record, root.path(), &policy, plenty).unwrap();
        assert!(layout.exists());
        assert_eq!(record.kept[0].because, KeptBecause::Recent);
        assert_eq!(record.kept[0].eligible, Some(NOW + 11 * DAY));

        // No copy, nothing recorded.
        let empty = tempfile::tempdir().unwrap();
        let mut record = Record::default();
        clean_vm_image(&mut record, empty.path(), &policy, plenty).unwrap();
        assert_eq!(record, Record::default());
    }

    #[test]
    fn old_test_fixtures_go_while_current_and_locked_ones_stay() {
        let policy = Policy::default();
        let plenty = conditions(Some(100_000_000_000));
        let root = tempfile::tempdir().unwrap();
        let ntfs = root.path().join("ntfs-fixtures");
        // The previous generator revision's set, untouched for a month.
        file(&ntfs.join("0ld0ld/disk.img"), 4096, 30);
        file(&ntfs.join("0ld0ld/manifest.json"), 10, 30);
        age_dir(&ntfs.join("0ld0ld"), 30);
        // The current one, read by yesterday's run.
        file(&ntfs.join("cur7e7/disk.img"), 4096, 1);
        // A loose file directly under the root.
        file(&root.path().join("gst-registry.bin"), 100, 40);
        // A media cache that a running test holds.
        let media = root.path().join("tilo-media-test-fixtures");
        file(&media.join("clip.webm"), 100, 40);
        let running = FileLock::open(&media.join(".lock"), LockMode::Exclusive).unwrap();

        let mut record = Record::default();
        clean_test_caches(&mut record, root.path(), &policy, plenty).unwrap();
        drop(running);

        assert!(!ntfs.join("0ld0ld").exists());
        assert!(!root.path().join("gst-registry.bin").exists());
        assert!(ntfs.join("cur7e7/disk.img").exists());
        assert!(media.join("clip.webm").exists());
        let removed: Vec<&str> = record.removed.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(removed, ["ntfs-fixtures/0ld0ld", "gst-registry.bin"]);
        let kept: Vec<(&str, KeptBecause)> = record
            .kept
            .iter()
            .map(|k| (k.name.as_str(), k.because))
            .collect();
        assert_eq!(
            kept,
            [
                ("ntfs-fixtures/cur7e7", KeptBecause::Recent),
                ("tilo-media-test-fixtures/clip.webm", KeptBecause::InUse),
            ]
        );
        // The locks themselves are never cleaned.
        assert!(ntfs.join(".lock").exists());
    }

    /// A stand-in `flatpak` that keeps the runtimes its "installed apps" name and removes the
    /// rest when asked for `uninstall --unused`, recording what it was asked.
    fn fake_flatpak(dir: &Path, installed: &[&str], used: &[&str]) -> Flatpak {
        let state = dir.join("runtimes");
        std::fs::write(&state, installed.join("\n") + "\n").unwrap();
        let keep = used.join("\\|");
        let program = dir.join("flatpak");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\ncase \"$1\" in\n\
                 list) cat {state} ;;\n\
                 uninstall) grep -x '{keep}' {state} > {state}.new; mv {state}.new {state} ;;\n\
                 esac\n",
                calls = dir.join("calls").display(),
                state = state.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = dir.join("flatpak-root");
        for reference in installed {
            file(
                &root.join("runtime").join(reference).join("files/lib.so"),
                2048,
                0,
            );
        }
        Flatpak { program, root }
    }

    #[test]
    fn flatpak_decides_which_runtimes_are_unused_and_apps_keep_theirs() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = fake_flatpak(
            dir.path(),
            &[
                "org.gnome.Platform/aarch64/48",
                "org.gnome.Platform/aarch64/51",
                "org.freedesktop.Platform/aarch64/25.08",
            ],
            // An installed app uses GNOME 51 and freedesktop 25.08.
            &[
                "org.gnome.Platform/aarch64/51",
                "org.freedesktop.Platform/aarch64/25.08",
            ],
        );
        let mut record = Record::default();
        clean_flatpak(&mut record, &flatpak, &Policy::default());
        assert_eq!(record.failed, []);
        assert_eq!(record.removed.len(), 1, "{record:?}");
        assert_eq!(record.removed[0].name, "org.gnome.Platform/aarch64/48");
        assert!(record.removed[0].bytes >= 2048);
        // The cleanup never names a runtime: Flatpak's own notion of unused decides.
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            [
                "list --system --runtime --columns=ref",
                "uninstall --system --unused --noninteractive -y",
                "list --system --runtime --columns=ref",
            ]
        );

        // Turned off by policy: Flatpak is not even asked.
        let off = Policy::parse("[flatpak]\nremove_unused_runtimes = false\n").unwrap();
        std::fs::remove_file(dir.path().join("calls")).unwrap();
        clean_flatpak(&mut Record::default(), &flatpak, &off);
        assert!(!dir.path().join("calls").exists());
    }

    /// A machine on bootc's composefs backend: deployments under `state/deploy`, boot
    /// entries naming some of them, and a bootc whose collection removes, as bootc's
    /// does, every deployment and repository object no boot entry names. It writes what it
    /// was asked and whether the update and ESP locks were held while it ran.
    fn fake_bootc(dir: &Path, deployments: &[&str], entries: &[&str]) -> Bootc {
        let sysroot = dir.join("sysroot");
        let esp = dir.join("esp");
        std::fs::create_dir_all(esp.join("loader/entries")).unwrap();
        for deployment in deployments {
            file(
                &sysroot
                    .join("state/deploy")
                    .join(deployment)
                    .join("etc/os-release"),
                64,
                0,
            );
            file(&sysroot.join("composefs/images").join(deployment), 4096, 0);
        }
        for entry in entries {
            std::fs::write(
                esp.join("loader/entries").join(format!("{entry}.conf")),
                format!("options composefs={entry}\n"),
            )
            .unwrap();
        }
        let update_lock = dir.join("run/update.lock");
        let program = dir.join("bootc");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\n\
                 for lock in {lock} {esp}; do\n\
                   if flock -n \"$lock\" true; then echo free >> {calls}; else echo held >> {calls}; fi\n\
                 done\n\
                 for d in {sysroot}/state/deploy/*; do\n\
                   id=${{d##*/}}\n\
                   grep -qx \"options composefs=$id\" {esp}/loader/entries/*.conf || rm -rf \"$d\" {sysroot}/composefs/images/$id\n\
                 done\n",
                calls = dir.join("calls").display(),
                lock = update_lock.display(),
                esp = esp.display(),
                sysroot = sysroot.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Bootc {
            program,
            sysroot,
            update_lock,
            esp: Some(esp),
            boot_mount: None,
        }
    }

    fn remaining(bootc: &Bootc) -> Vec<String> {
        let mut left: Vec<String> = std::fs::read_dir(bootc.sysroot.join("state/deploy"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        left
    }

    #[test]
    fn os_images_no_deployment_boots_go_and_the_current_and_previous_stay() {
        let dir = tempfile::tempdir().unwrap();
        // The running generation, the rollback, and two older ones whose boot entries an
        // update replaced, plus the half of an update that was cancelled.
        let bootc = fake_bootc(
            dir.path(),
            &["current", "previous", "older", "oldest", "cancelled"],
            &["current", "previous"],
        );
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &Policy::default());
        assert_eq!(record.failed, []);
        assert_eq!(remaining(&bootc), ["current", "previous"]);
        assert!(bootc.sysroot.join("composefs/images/current").exists());
        assert!(bootc.sysroot.join("composefs/images/previous").exists());
        assert!(!bootc.sysroot.join("composefs/images/older").exists());
        // bootc decides what no boot entry names, while no update and no boot entry
        // rename can run.
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            ["internals composefs-gc --prune-repo", "held", "held"]
        );
        // The lock is created as the update requires it: owner-only.
        let mode = std::fs::metadata(&bootc.update_lock)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // A second run finds nothing more to remove and keeps both.
        clean_os_images(&mut Record::default(), &bootc, &Policy::default());
        assert_eq!(remaining(&bootc), ["current", "previous"]);
    }

    #[test]
    fn an_update_or_a_boot_entry_rename_in_progress_keeps_the_os_images_for_later() {
        let dir = tempfile::tempdir().unwrap();
        let bootc = fake_bootc(dir.path(), &["current", "older"], &["current"]);
        let calls = dir.path().join("calls");

        // An update holds its lock from download to activation.
        let update = try_hold_update_lock(&bootc.update_lock).unwrap().unwrap();
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &Policy::default());
        assert!(!calls.exists(), "bootc ran during an update");
        assert_eq!(record.kept.len(), 1);
        assert_eq!(record.kept[0].because, KeptBecause::InUse);
        assert_eq!(remaining(&bootc), ["current", "older"]);
        drop(update);

        // tilo-deployment renames boot entries under the ESP's lock.
        let esp = try_hold_directory(bootc.esp.as_deref().unwrap())
            .unwrap()
            .unwrap();
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &Policy::default());
        assert!(!calls.exists(), "bootc ran while boot entries were renamed");
        assert_eq!(record.kept[0].because, KeptBecause::InUse);
        drop(esp);

        // Turned off by policy: bootc is not asked.
        let off = Policy::parse("[os_images]\nremove = false\n").unwrap();
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &off);
        assert!(!calls.exists());
        assert_eq!(record.kept[0].because, KeptBecause::Disabled);

        // Once nothing holds it, the next run collects.
        clean_os_images(&mut Record::default(), &bootc, &Policy::default());
        assert_eq!(remaining(&bootc), ["current"]);
    }

    #[test]
    fn a_machine_without_a_composefs_repository_has_no_os_images_to_collect() {
        let dir = tempfile::tempdir().unwrap();
        let mut bootc = fake_bootc(dir.path(), &[], &[]);
        bootc.sysroot = dir.path().join("elsewhere");
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &Policy::default());
        assert_eq!(record, Record::default());
        assert!(!dir.path().join("calls").exists());
    }

    #[test]
    fn a_failed_os_image_collection_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut bootc = fake_bootc(dir.path(), &["current"], &["current"]);
        let program = dir.path().join("failing-bootc");
        std::fs::write(
            &program,
            "#!/bin/sh\necho 'error: repository is locked' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        bootc.program = program;
        let mut record = Record::default();
        clean_os_images(&mut record, &bootc, &Policy::default());
        assert_eq!(record.failed.len(), 1);
        assert_eq!(record.failed[0].kind, Kind::OsImage);
        assert_eq!(record.failed[0].error, "error: repository is locked");
    }

    #[test]
    fn status_says_what_was_freed_kept_and_when_it_can_go() {
        let record = Record {
            finished: NOW - 3 * 3600,
            removed: vec![
                Removed {
                    kind: Kind::FlatpakRuntime,
                    name: "org.gnome.Platform/aarch64/48".into(),
                    bytes: 900_000_000,
                },
                Removed {
                    kind: Kind::TestCache,
                    name: "ntfs-fixtures/0ld0ld".into(),
                    bytes: 300_000_000,
                },
            ],
            kept: vec![
                Kept {
                    kind: Kind::VmImage,
                    name: "the copy of the last developer image upgrade".into(),
                    bytes: 4_100_000_000,
                    because: KeptBecause::Recent,
                    eligible: Some(NOW + 9 * DAY),
                },
                Kept {
                    kind: Kind::TestCache,
                    name: "tilo-media-test-fixtures/clip.webm".into(),
                    bytes: 2_000_000,
                    because: KeptBecause::InUse,
                    eligible: None,
                },
            ],
            failed: vec![Failed {
                kind: Kind::FlatpakRuntime,
                name: "unused Flatpak runtimes".into(),
                error: "error: database locked".into(),
            }],
        };
        assert_eq!(
            render("Disk cleanup", Some(&record), at(NOW)),
            "Disk cleanup, last run 3 hours ago:\n\
             \x20 Freed 1.2 GB: removed Flatpak runtime org.gnome.Platform 48, test cache ntfs-fixtures/0ld0ld.\n\
             \x20 Kept the copy of the last developer image upgrade (4.1 GB): used recently; it can go in 9 days.\n\
             \x20 Kept test cache tilo-media-test-fixtures/clip.webm (2 MB): it was in use.\n\
             \x20 Could not remove unused Flatpak runtimes: error: database locked\n"
        );
        assert_eq!(
            render(
                "Test caches",
                Some(&Record {
                    finished: NOW - 30 * 3600,
                    ..Record::default()
                }),
                at(NOW)
            ),
            "Test caches, last run yesterday:\n  Nothing needed removing.\n"
        );
        assert_eq!(
            render("Disk cleanup", None, at(NOW)),
            "Disk cleanup: has not run yet; it runs daily.\n"
        );
    }

    #[test]
    fn the_record_round_trips_and_is_readable_by_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cache-gc.json");
        let record = Record {
            finished: NOW,
            removed: vec![Removed {
                kind: Kind::VmImage,
                name: "x".into(),
                bytes: 1,
            }],
            ..Record::default()
        };
        write_record(&path, &record).unwrap();
        assert_eq!(read_record(&path), Some(record));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}

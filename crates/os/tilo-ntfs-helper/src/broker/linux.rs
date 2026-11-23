#[path = "ipc.rs"]
mod ipc;
#[path = "session.rs"]
mod session;
use crate::limits;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Candidate {
    inventory_id: String,
    name: String,
    sysfs: PathBuf,
    hardware: String,
    major: u32,
    minor: u32,
    start: u64,
    sectors: u64,
    disk_sequence: u64,
    sysfs_inode: u64,
}
fn refusal(reason: &str) -> io::Error {
    io::Error::other(reason)
}
fn text(path: &Path) -> io::Result<String> {
    let s = fs::read_to_string(path)?;
    if s.len() > 4096 {
        return Err(refusal("oversized native inventory field"));
    }
    Ok(s.trim().to_owned())
}
fn number(path: &Path) -> io::Result<u64> {
    text(path)?.parse().map_err(io::Error::other)
}
fn digest(fields: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"tilo-ntfs-stage1-inventory-v1\0");
    for field in fields {
        digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(field);
    }
    hex::encode(digest.finalize())
}
fn candidate(path: &Path) -> io::Result<Candidate> {
    let sysfs = fs::canonicalize(path)?;
    if !sysfs.starts_with("/sys/devices") || sysfs.starts_with("/sys/devices/virtual") {
        return Err(refusal("unsupported device topology"));
    }
    let name = sysfs
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| refusal("invalid native device name"))?
        .to_owned();
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(refusal("unsupported device name"));
    }
    // Stage 1 intentionally supports physical partitions only. Whole disks and loops cannot be production identities.
    let _partition = number(&sysfs.join("partition"))?;
    let parent = sysfs
        .parent()
        .ok_or_else(|| refusal("missing parent device"))?;
    super::topology::no_holders(parent, true)?;
    super::topology::no_holders(&sysfs, false)?;
    let hardware = text(&parent.join("wwid")).or_else(|_| text(&parent.join("device/wwid")))?;
    if hardware.is_empty() {
        return Err(refusal("missing native hardware identifier"));
    }
    let major_minor = text(&sysfs.join("dev"))?;
    let (major, minor) = major_minor
        .split_once(':')
        .ok_or_else(|| refusal("invalid native device number"))?;
    let major = major.parse().map_err(io::Error::other)?;
    let minor = minor.parse().map_err(io::Error::other)?;
    let start = number(&sysfs.join("start"))?;
    let sectors = number(&sysfs.join("size"))?;
    let disk_sequence = number(&parent.join("diskseq"))?;
    let sysfs_inode = fs::metadata(&sysfs)?.ino();
    let inventory_id = digest(&[
        hardware.as_bytes(),
        &start.to_le_bytes(),
        &sectors.to_le_bytes(),
        &disk_sequence.to_le_bytes(),
        name.as_bytes(),
    ]);
    Ok(Candidate {
        inventory_id,
        name,
        sysfs,
        hardware,
        major,
        minor,
        start,
        sectors,
        disk_sequence,
        sysfs_inode,
    })
}
fn non_ntfs_mounts() -> io::Result<BTreeSet<String>> {
    use std::io::Read;
    let mut bytes = String::new();
    File::open("/proc/self/mountinfo")?
        .take(1_048_577)
        .read_to_string(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(refusal("mount inventory limit"));
    }
    let mut result = BTreeSet::new();
    for line in bytes.lines() {
        let (left, right) = line
            .split_once(" - ")
            .ok_or_else(|| refusal("invalid native mount inventory"))?;
        let fs = right
            .split_whitespace()
            .next()
            .ok_or_else(|| refusal("missing filesystem type"))?;
        if [
            "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "exfat", "f2fs", "squashfs", "iso9660",
        ]
        .contains(&fs)
        {
            result.insert(
                left.split_whitespace()
                    .nth(2)
                    .ok_or_else(|| refusal("missing mounted device number"))?
                    .to_owned(),
            );
        }
    }
    Ok(result)
}
fn inventory() -> io::Result<(Vec<Candidate>, Vec<String>)> {
    let other_mounted = non_ntfs_mounts()?;
    let mut entries = Vec::new();
    let mut unavailable = Vec::new();
    for entry in fs::read_dir("/sys/class/block")? {
        let entry = entry?;
        if entries.len().saturating_add(unavailable.len()) >= 1024 {
            return Err(refusal("inventory limit"));
        }
        if !entry.path().join("partition").exists() {
            continue;
        }
        // Native kernel filesystem type only classifies known non-NTFS holders; it never proves an NTFS claim.
        if other_mounted.contains(&text(&entry.path().join("dev"))?) {
            continue;
        }
        match candidate(&entry.path()) {
            Ok(c) => entries.push(c),
            Err(e) => unavailable.push(format!("{}: {e}", entry.file_name().to_string_lossy())),
        }
    }
    entries.sort_by(|a, b| a.inventory_id.cmp(&b.inventory_id));
    Ok((entries, unavailable))
}
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Descriptor {
    rdev: u64,
    bytes: u64,
    sector_size: i32,
    media_readonly: i32,
    disk_sequence: u64,
}
fn descriptor(file: &File, c: &Candidate) -> io::Result<Descriptor> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_block_device() || metadata.rdev() != libc::makedev(c.major, c.minor)
    {
        return Err(refusal("descriptor is not selected native block device"));
    }
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(refusal("descriptor is not readonly"));
    }
    let mut bytes = 0_u64;
    let mut sector_size = 0_i32;
    let mut media_readonly = 0_i32;
    let mut disk_sequence = 0_u64;
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x8008_1272_u64, &raw mut bytes) } != 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x1268_u64, &raw mut sector_size) } != 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x125e_u64, &raw mut media_readonly) } != 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x8008_1280_u64, &raw mut disk_sequence) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if bytes
        != c.sectors
            .checked_mul(512)
            .ok_or_else(|| refusal("geometry overflow"))?
        || disk_sequence != c.disk_sequence
        || !(512..=4096).contains(&sector_size)
        || !u32::try_from(sector_size)
            .map_err(io::Error::other)?
            .is_power_of_two()
        || !(0..=1).contains(&media_readonly)
    {
        return Err(refusal("native geometry changed or unsupported"));
    }
    Ok(Descriptor {
        rdev: metadata.rdev(),
        bytes,
        sector_size,
        media_readonly,
        disk_sequence,
    })
}
fn credentials() -> io::Result<(u32, u32)> {
    let name = CString::new("tilo-ntfs").map_err(io::Error::other)?;
    // Single-threaded administrator command; copied before any fork and never used after NSS memory changes.
    let account = unsafe { libc::getpwnam(name.as_ptr()) };
    if account.is_null() {
        return Err(refusal(
            "dedicated tilo-ntfs account is required; service activation is not part of Stage 1",
        ));
    }
    let (uid, gid) = unsafe { ((*account).pw_uid, (*account).pw_gid) };
    if uid == 0 || gid == 0 {
        return Err(refusal("reader identity must be unprivileged"));
    }
    Ok((uid, gid))
}
pub(super) fn command(args: &[String]) -> io::Result<()> {
    // Overall administrative deadline also covers native inventory/claim setup.
    unsafe {
        libc::alarm(20);
    }
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "explicit administrative invocation required",
        ));
    }
    if args == ["inventory"] {
        let (entries, unavailable) = inventory()?;
        println!(
            "{}",
            serde_json::json!({"schema":1,"inventory":entries,"unavailable":unavailable,"diagnostic_only":true})
        );
        return Ok(());
    }
    if args.first().is_some_and(|s| s == "serve-read-selected") {
        return session::serve(args);
    }
    let (id, root) = selection(args)?;
    let identity = credentials()?;
    let (entries, unavailable) = inventory()?;
    let selected = entries
        .iter()
        .find(|c| c.inventory_id == id)
        .ok_or_else(|| refusal("selected inventory object unavailable"))?;
    let mut names = BTreeSet::new();
    for c in &entries {
        if !names.insert((&c.hardware, c.start, c.sectors)) {
            return Err(refusal("ambiguous hardware and partition identity"));
        }
    }
    if !unavailable.is_empty() {
        return Err(refusal("ambiguous: unresolved native inventory candidates"));
    }
    let mounts_before = non_ntfs_mounts()?;
    let (claims, selected_index) = claim_candidates(&entries, id)?;
    let owned = claims
        .get(selected_index)
        .ok_or_else(|| refusal("selected claim lost"))?;
    let claim = &owned.device;
    let before = &owned.observation;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(limits::WALL_SECONDS))
        .ok_or_else(|| refusal("wall deadline overflow"))?;
    let inventory_reads = u64::try_from(claims.len())
        .map_err(io::Error::other)?
        .checked_mul(8192)
        .ok_or_else(|| refusal("inventory read budget overflow"))?;
    let reader_budget = limits::READ_BYTES
        .checked_sub(inventory_reads)
        .ok_or_else(|| refusal("inventory read budget"))?;
    let first = limits::run(
        claim,
        before.bytes,
        root,
        reader_budget,
        Some(identity),
        deadline,
    )?;
    if first.status != 0 {
        return Err(refusal(&format!(
            "native admission refused with status {}",
            first.status
        )));
    }
    if !first.facts.iter().any(|v| {
        v.get("kind").and_then(serde_json::Value::as_str) == Some("volume")
            && v.get("sector_size").and_then(serde_json::Value::as_i64)
                == Some(i64::from(before.sector_size))
    }) {
        return Err(refusal("native sector geometry disagrees with descriptor"));
    }
    let remaining = reader_budget
        .checked_sub(first.stats.bytes)
        .ok_or_else(|| refusal("aggregate read budget"))?;
    let second = limits::run(
        claim,
        before.bytes,
        root,
        remaining,
        Some(identity),
        deadline,
    )?;
    if second.status != 0
        || first.facts != second.facts
        || *before != descriptor(claim, selected)?
        || candidate(&selected.sysfs)? != *selected
    {
        return Err(refusal(
            "claim invalidated: native evidence or hardware changed",
        ));
    }
    revalidate(&claims)?;
    let (after, unresolved) = inventory()?;
    if after != entries || !unresolved.is_empty() || non_ntfs_mounts()? != mounts_before {
        return Err(refusal("native inventory changed during inspection"));
    }
    publish(selected, before, &first, root)
}

struct HeldClaim {
    device: File,
    _sysfs: File,
    candidate: Candidate,
    observation: Descriptor,
    boot: [u8; 4096],
}
fn claim_candidates(entries: &[Candidate], id: &str) -> io::Result<(Vec<HeldClaim>, usize)> {
    let dev = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/dev")?;
    let mut claims = Vec::new();
    let mut serials = BTreeMap::new();
    let mut selected_index = None;
    for c in entries {
        let sysfs = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&c.sysfs)?;
        let name = CString::new(c.name.as_bytes()).map_err(io::Error::other)?;
        let fd = unsafe {
            libc::openat(
                dev.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_EXCL,
            )
        };
        if fd < 0 {
            return Err(refusal(&format!(
                "Busy or unresolved candidate: {}",
                io::Error::last_os_error()
            )));
        }
        let claim = unsafe { File::from_raw_fd(fd) };
        let observation = descriptor(&claim, c)?;
        let mut bytes = [0_u8; 4096];
        claim.read_exact_at(&mut bytes, 0)?;
        let magic = bytes
            .get(3..11)
            .ok_or_else(|| refusal("native signature missing"))?;
        if magic == b"NTFS    " {
            let serial = u64::from_le_bytes(
                bytes
                    .get(72..80)
                    .ok_or_else(|| refusal("native serial missing"))?
                    .try_into()
                    .map_err(io::Error::other)?,
            );
            if serials.insert(serial, c.inventory_id.clone()).is_some() {
                return Err(refusal(
                    "ambiguous: duplicate native NTFS serial, independent of health",
                ));
            }
            if c.inventory_id == id {
                selected_index = Some(claims.len());
            }
        } else {
            let known_other = magic == b"EXFAT   "
                || bytes.get(82..90) == Some(b"FAT32   ")
                || bytes.get(54..62) == Some(b"FAT16   ")
                || bytes.get(1080..1082) == Some(&[0x53, 0xef])
                || bytes.get(..4) == Some(b"XFSB");
            if !known_other {
                return Err(refusal(
                    "ambiguous: unknown or locked native candidate signature",
                ));
            }
        }
        claims.push(HeldClaim {
            device: claim,
            _sysfs: sysfs,
            candidate: c.clone(),
            observation,
            boot: bytes,
        });
    }
    let selected_index =
        selected_index.ok_or_else(|| refusal("selected volume is not native NTFS"))?;
    Ok((claims, selected_index))
}

fn selection(args: &[String]) -> io::Result<(&str, u64)> {
    let [verb, id_flag, id, root_flag, root] = args else {
        return Err(refusal(
            "expected inventory or inspect-selected --inventory-id ID --root-mft RECORD",
        ));
    };
    if verb != "inspect-selected"
        || id_flag != "--inventory-id"
        || root_flag != "--root-mft"
        || id.len() != 64
        || !id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(refusal(
            "invalid administrative selection; paths and caller assertions are forbidden",
        ));
    }
    let root: u64 = root.parse().map_err(io::Error::other)?;
    if root > 0x0000_ffff_ffff_ffff {
        return Err(refusal("invalid MFT locator"));
    }
    Ok((id, root))
}

fn publish(
    selected: &Candidate,
    before: &Descriptor,
    first: &crate::protocol::Report,
    root: u64,
) -> io::Result<()> {
    let native = crate::protocol::identity(first, root)?;
    let hardware_volume = digest(&[
        b"persistent-volume-v1",
        selected.hardware.as_bytes(),
        &selected.start.to_le_bytes(),
        &selected.sectors.to_le_bytes(),
        native.volume.as_bytes(),
    ]);
    let persistent_root = digest(&[
        b"persistent-root-v1",
        hardware_volume.as_bytes(),
        native.root.as_bytes(),
    ]);
    println!(
        "{}",
        serde_json::json!({"schema":1,"diagnostic_only":true,"inventory":selected,"descriptor":before,"native_identity":native,"persistent_volume":hardware_volume,"persistent_root":persistent_root,"partition_identifier":null,"claim_lifetime":"this-command-only","report":first})
    );
    Ok(())
}

fn revalidate(claims: &[HeldClaim]) -> io::Result<()> {
    for owned in claims {
        let fd = &owned.device;
        let c = &owned.candidate;
        let observation = &owned.observation;
        let boot = &owned.boot;
        let mut after = [0_u8; 4096];
        fd.read_exact_at(&mut after, 0)?;
        if after != *boot || descriptor(fd, c)? != *observation || candidate(&c.sysfs)? != *c {
            return Err(refusal("candidate replacement invalidated clone inventory"));
        }
    }
    Ok(())
}

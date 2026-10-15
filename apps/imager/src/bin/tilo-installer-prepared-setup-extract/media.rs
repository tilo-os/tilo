use super::{Result, fs, invalid};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::process::Command;
use std::time::Duration;

const EFI: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const HOME: &str = "7c4f89e9-0bd4-45ac-8ea6-319ab6795a91";
pub fn command(program: &str, args: &[&str], bound: usize, seconds: u64) -> Result<Vec<u8>> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C");
    let out = bounded_command::run_prepared(cmd, None, bound, Duration::from_secs(seconds))
        .map_err(|_| invalid())?;
    if !out.status.success() {
        return Err(invalid());
    }
    Ok(out.stdout)
}
fn block(path: &str) -> Result<Metadata> {
    let m = std::fs::symlink_metadata(path)?;
    if !m.file_type().is_block_device() || m.uid() != 0 {
        return Err(invalid());
    }
    Ok(m)
}
fn dev_identity(m: &Metadata) -> Value {
    json!([m.dev(), m.ino(), m.rdev()])
}
pub fn inventory(device: &str) -> Result<Value> {
    let before = block(device)?;
    let data = command(
        "/usr/bin/lsblk",
        &[
            "--json",
            "--bytes",
            "--paths",
            "--output",
            "NAME,TYPE,FSTYPE,PARTTYPE,PARTLABEL,PARTUUID,UUID,MAJ:MIN,MOUNTPOINTS",
            device,
        ],
        4 * 1024 * 1024,
        10,
    )?;
    let value: Value = serde_json::from_slice(&data).map_err(|_| invalid())?;
    let rows = value
        .get("blockdevices")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if rows.len() != 1
        || rows
            .first()
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
            != Some(device)
        || dev_identity(&before) != dev_identity(&block(device)?)
    {
        return Err(invalid());
    }
    rows.first().cloned().ok_or_else(invalid)
}
fn rows(value: &Value) -> Vec<&Value> {
    value
        .get("children")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}
fn mounted(value: &Value, depth: usize, roots: &mut BTreeSet<String>) -> Result<()> {
    if depth > 16 {
        return Err(invalid());
    }
    if let Some(points) = value.get("mountpoints").and_then(Value::as_array) {
        for point in points {
            if let Some(p) = point.as_str().filter(|p| p.starts_with('/')) {
                roots.insert(p.to_owned());
            }
        }
    }
    for c in rows(value) {
        mounted(c, depth.checked_add(1).ok_or_else(invalid)?, roots)?;
    }
    Ok(())
}
fn part_type(part: &Value) -> String {
    part.get("parttype")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}
fn valid(part: &Value) -> bool {
    (part_type(part) == HOME
        && part.get("fstype").and_then(Value::as_str) == Some("ext4")
        && part.get("partlabel").and_then(Value::as_str) == Some("Tilo migration"))
        || (part_type(part) == EFI && part.get("fstype").and_then(Value::as_str) == Some("vfat"))
}
fn dev_path(v: &Value) -> Result<&str> {
    let p = v.as_str().ok_or_else(invalid)?;
    if !p.starts_with("/dev/")
        || std::path::Path::new(p).parent() != Some(std::path::Path::new("/dev"))
    {
        return Err(invalid());
    }
    Ok(p)
}
struct Lease {
    path: String,
    original: (u64, u64),
    device: u64,
}
impl Lease {
    fn retire(&self) -> Result<()> {
        let mut m = std::fs::symlink_metadata(&self.path)?;
        if m.dev() == self.device {
            command("/usr/bin/umount", &[&self.path], 65536, 15)?;
            m = std::fs::symlink_metadata(&self.path)?;
        }
        if fs::identity(&m) != self.original {
            return Err(invalid());
        }
        std::fs::remove_dir(&self.path)
    }
}
pub struct Grants {
    pub roots: Vec<String>,
    leases: Vec<Lease>,
}
impl Grants {
    pub fn acquire(devices: &[String]) -> Result<Self> {
        let mut grants = Self {
            roots: Vec::new(),
            leases: Vec::new(),
        };
        let mut roots = BTreeSet::new();
        let unique: BTreeSet<_> = devices.iter().collect();
        for device in unique {
            let row = inventory(device)?;
            mounted(&row, 0, &mut roots)?;
            if row.get("type").and_then(Value::as_str) != Some("disk") {
                continue;
            }
            let efi: Vec<_> = rows(&row)
                .into_iter()
                .filter(|p| part_type(p) == EFI)
                .collect();
            let homes: Vec<_> = rows(&row)
                .into_iter()
                .filter(|p| part_type(p) == HOME)
                .collect();
            if efi.len() > 1 || homes.len() > 1 {
                return Err(invalid());
            }
            for p in efi.into_iter().chain(homes) {
                if p.get("mountpoints")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| !v.is_null() && v != ""))
                {
                    continue;
                }
                if !valid(p)
                    || p.get("type").and_then(Value::as_str) != Some("part")
                    || !rows(p).is_empty()
                    || p.get("uuid")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || p.get("partuuid")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                {
                    return Err(invalid());
                }
                let name = dev_path(p.get("name").ok_or_else(invalid)?)?;
                let before = block(name)?;
                let number = format!(
                    "{}:{}",
                    libc::major(before.rdev()),
                    libc::minor(before.rdev())
                );
                if p.get("maj:min").and_then(Value::as_str) != Some(number.as_str()) {
                    return Err(invalid());
                }
                let root = tempfile::Builder::new()
                    .prefix("tilo-setup-media-")
                    .tempdir()?
                    .keep();
                let root = root.to_str().ok_or_else(invalid)?.to_owned();
                let original = fs::identity(&std::fs::symlink_metadata(&root)?);
                grants.leases.push(Lease {
                    path: root.clone(),
                    original,
                    device: before.rdev(),
                });
                let home = part_type(p) == HOME;
                let fstype = if home { "ext4" } else { "vfat" };
                let options = if home {
                    "ro,nodev,nosuid,noexec,noload"
                } else {
                    "ro,nodev,nosuid,noexec,umask=0077"
                };
                command(
                    "/usr/bin/mount",
                    &["-t", fstype, "-o", options, name, &root],
                    65536,
                    15,
                )?;
                let current = inventory(device)?;
                let matches: Vec<_> = rows(&current)
                    .into_iter()
                    .filter(|v| v.get("name") == p.get("name"))
                    .collect();
                if matches.len() != 1
                    || matches.first().is_none_or(|v| {
                        v.get("uuid") != p.get("uuid") || v.get("partuuid") != p.get("partuuid")
                    })
                    || dev_identity(&block(name)?) != dev_identity(&before)
                    || std::fs::symlink_metadata(&root)?.dev() != before.rdev()
                    || !fs::capacity(&fs::directory(&root)?)?.1
                {
                    return Err(invalid());
                }
                roots.insert(root);
            }
        }
        grants.roots = roots.into_iter().collect();
        Ok(grants)
    }
    pub fn release(&mut self) -> Result<()> {
        let mut failure = None;
        for lease in self.leases.drain(..).rev() {
            if let Err(e) = lease.retire()
                && failure.is_none()
            {
                failure = Some(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for Grants {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
pub fn binding(root: &str, devices: &[String]) -> Result<Value> {
    let fd = fs::directory(root)?;
    let m = fd.metadata()?;
    if devices.is_empty() {
        return Ok(json!({"kind":"directory","path":root,"device":m.dev(),"inode":m.ino()}));
    }
    for device in devices {
        let row = inventory(device)?;
        let disk = block(device)?;
        for p in rows(&row) {
            let Ok(name) = dev_path(p.get("name").unwrap_or(&Value::Null)) else {
                continue;
            };
            let part = block(name)?;
            if part.rdev() == m.dev() {
                if !valid(p)
                    || p.get("uuid")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || p.get("partuuid")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || !fs::capacity(&fd)?.1
                {
                    return Err(invalid());
                }
                return Ok(
                    json!({"kind":"filesystem","disk":device,"disk_identity":dev_identity(&disk),"partition":name,"partition_identity":dev_identity(&part),"uuid":p.get("uuid"),"partuuid":p.get("partuuid"),"fstype":p.get("fstype")}),
                );
            }
        }
    }
    Err(invalid())
}
pub fn with_root<T>(binding: &Value, f: impl FnOnce(&File) -> Result<T>) -> Result<T> {
    let object = binding.as_object().ok_or_else(invalid)?;
    if binding.get("kind").and_then(Value::as_str) == Some("directory") {
        if object.len() != 4
            || !["kind", "path", "device", "inode"]
                .iter()
                .all(|k| object.contains_key(*k))
        {
            return Err(invalid());
        }
        let fd = fs::directory(
            binding
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?,
        )?;
        let m = fd.metadata()?;
        if binding.get("device").and_then(Value::as_u64) != Some(m.dev())
            || binding.get("inode").and_then(Value::as_u64) != Some(m.ino())
        {
            return Err(invalid());
        }
        return f(&fd);
    }
    let keys = [
        "kind",
        "disk",
        "disk_identity",
        "partition",
        "partition_identity",
        "uuid",
        "partuuid",
        "fstype",
    ];
    if binding.get("kind").and_then(Value::as_str) != Some("filesystem")
        || object.len() != keys.len()
        || !keys.iter().all(|k| object.contains_key(*k))
    {
        return Err(invalid());
    }
    let disk = binding
        .get("disk")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let part = binding
        .get("partition")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let dm = block(disk)?;
    let pm = block(part)?;
    if binding.get("disk_identity") != Some(&dev_identity(&dm))
        || binding.get("partition_identity") != Some(&dev_identity(&pm))
    {
        return Err(invalid());
    }
    let row = inventory(disk)?;
    let matches: Vec<_> = rows(&row)
        .into_iter()
        .filter(|v| v.get("name").and_then(Value::as_str) == Some(part))
        .collect();
    if matches.len() != 1
        || matches.first().is_none_or(|p| {
            ["uuid", "partuuid", "fstype"]
                .iter()
                .any(|k| p.get(k) != binding.get(k))
        })
    {
        return Err(invalid());
    }
    let mut grants = Grants::acquire(&[disk.to_owned()])?;
    let mut candidates = Vec::new();
    for root in &grants.roots {
        let fd = fs::directory(root)?;
        if fd.metadata()?.dev() == pm.rdev() {
            candidates.push(fd);
        }
    }
    let result = if candidates.len() == 1 {
        let fd = candidates.first().ok_or_else(invalid)?;
        if fs::capacity(fd)?.1 {
            f(fd)
        } else {
            Err(invalid())
        }
    } else {
        Err(invalid())
    };
    drop(candidates);
    let release = grants.release();
    result.and_then(|v| release.map(|()| v))
}

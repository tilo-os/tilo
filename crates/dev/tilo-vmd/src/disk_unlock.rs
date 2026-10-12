//! Enrollment of the kernel's current encrypted root, never a caller-selected disk.

use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_vm_protocol::{DeviceUnlockEnrol, DeviceUnlockEnrolResult};

use crate::error::DaemonError;
use crate::exec::{Ended, run_bounded};

const HELPER: &str = "/usr/bin/tilo-sibling-unlock";

#[derive(serde::Deserialize)]
struct Status {
    keyslot: u32,
    sealing: String,
    networks: Vec<String>,
    siblings: Vec<String>,
}

fn refusal() -> DaemonError {
    DaemonError::Apply {
        step: "device unlock enrollment".into(),
        detail: "enrollment could not be completed for the current encrypted root".into(),
    }
}

struct RootMount<'a> {
    number: &'a str,
    filesystem: &'a str,
    source: &'a str,
}

fn root_mount(mounts: &str) -> Option<RootMount<'_>> {
    let mut root = None;
    for line in mounts.lines() {
        let (head, tail) = line.split_once(" - ")?;
        let mut fields = head.split_whitespace();
        let number = fields.nth(2)?;
        let mountpoint = fields.nth(1)?;
        if !matches!(mountpoint, "/" | "/sysroot") {
            continue;
        }
        let mut tail = tail.split_whitespace();
        let mount = RootMount {
            number,
            filesystem: tail.next()?,
            source: tail.next()?,
        };
        if mountpoint == "/sysroot" {
            return Some(mount);
        }
        root = Some(mount);
    }
    root
}

// Btrfs mountinfo dev_t identifies a subvolume, rather than its block device.
// Corroborate the kernel-reported source against the mounted filesystem's
// devices and devinfo, including missing devices; a degraded multi-device
// filesystem must not be mistaken for a single-root volume.
// https://btrfs.readthedocs.io/en/stable/ch-sysfs.html
fn single_btrfs_device(root: &Path, number: &str) -> std::io::Result<bool> {
    let mut matched = false;
    for filesystem in std::fs::read_dir(root)? {
        let filesystem = filesystem?;
        if uuid::Uuid::parse_str(&filesystem.file_name().to_string_lossy()).is_err() {
            continue;
        }
        let directory = filesystem.path();
        let devices = std::fs::read_dir(directory.join("devices"))?
            .map(|entry| std::fs::read_to_string(entry?.path().join("dev")))
            .collect::<std::io::Result<Vec<_>>>()?;
        if devices.iter().any(|device| device.trim() == number) {
            let registered = std::fs::read_dir(directory.join("devinfo"))?
                .collect::<std::io::Result<Vec<_>>>()?;
            if devices.len() != 1 || registered.len() != 1 {
                return Ok(false);
            }
            if matched {
                return Ok(false);
            }
            matched = true;
        }
    }
    Ok(matched)
}

fn mounted_mapper_number(mount: &RootMount<'_>, btrfs_sys: &Path) -> std::io::Result<String> {
    if !mount.source.starts_with("/dev/") || mount.source.contains('\\') {
        return Err(std::io::Error::other(
            "root mount has no unambiguous block source",
        ));
    }
    let source = std::fs::canonicalize(mount.source)?;
    let metadata = source.metadata()?;
    if !metadata.file_type().is_block_device() {
        return Err(std::io::Error::other("root source is not a block device"));
    }
    let number = format!(
        "{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    );
    if mount.filesystem == "btrfs" {
        if !single_btrfs_device(btrfs_sys, &number)? {
            return Err(std::io::Error::other(
                "root filesystem does not have one proven backing device",
            ));
        }
    } else if mount.number != number {
        return Err(std::io::Error::other(
            "root source disagrees with its mounted block identity",
        ));
    }
    Ok(number)
}

fn current_volume() -> std::io::Result<(PathBuf, String)> {
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mount = root_mount(&mounts).ok_or_else(|| std::io::Error::other("root not found"))?;
    let number = mounted_mapper_number(&mount, Path::new("/sys/fs/btrfs"))?;
    let sys = Path::new("/sys/dev/block").join(number);
    let dm = std::fs::read_to_string(sys.join("dm/uuid"))?;
    let uuid = dm
        .trim()
        .strip_prefix("CRYPT-LUKS2-")
        .and_then(|value| value.split('-').next())
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| std::io::Error::other("root is not a LUKS2 mapper"))?;
    let mut slaves = std::fs::read_dir(sys.join("slaves"))?;
    let slave = slaves
        .next()
        .transpose()?
        .ok_or_else(|| std::io::Error::other("root backing device absent"))?;
    if slaves.next().is_some() {
        return Err(std::io::Error::other("root backing device is ambiguous"));
    }
    let backing = std::fs::read_to_string(slave.path().join("dev"))?;
    let path = std::fs::canonicalize(Path::new("/dev/block").join(backing.trim()))?;
    let metadata = path.metadata()?;
    let expected = backing
        .trim()
        .split_once(':')
        .and_then(|(major, minor)| Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?)))
        .ok_or_else(|| std::io::Error::other("invalid block identity"))?;
    if !metadata.file_type().is_block_device()
        || libc::major(metadata.rdev()) != expected.0
        || libc::minor(metadata.rdev()) != expected.1
    {
        return Err(std::io::Error::other("backing block identity mismatch"));
    }
    let uuid = uuid::Uuid::parse_str(uuid)
        .map_err(|_| std::io::Error::other("invalid LUKS2 UUID"))?
        .hyphenated()
        .to_string();
    Ok((path, uuid))
}

fn private_key_file(passphrase: &str) -> std::io::Result<File> {
    // SAFETY: static terminated name and flags accepted by memfd_create.
    let fd = unsafe {
        libc::memfd_create(
            c"tilo-device-unlock".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: this newly-created descriptor is exclusively owned here.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(passphrase.as_bytes())?;
    file.rewind()?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: valid owned descriptor and integer seal mask.
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

fn inherit_private_key(command: &mut Command, key: &File) {
    let fd = key.as_raw_fd();
    // SAFETY: only async-signal-safe fcntl is called after fork. The parent
    // descriptor remains CLOEXEC; exactly this sealed descriptor is inherited.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("output unavailable"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn account_is_admin(user: &str) -> bool {
    let output = Output::default();
    let mut command = Command::new("/usr/bin/id");
    command.args(["-nG", "--", user]);
    if run_bounded(
        command,
        Duration::from_secs(5),
        65536,
        output.clone(),
        std::io::sink(),
    )
    .ok()
        != Some(Ended::Exited(0))
    {
        return false;
    }
    output.0.lock().ok().is_some_and(|bytes| {
        String::from_utf8_lossy(&bytes)
            .split_whitespace()
            .any(|group| group == "wheel")
    })
}

pub(crate) fn enroll(
    uid: u32,
    request: &DeviceUnlockEnrol,
) -> Result<DeviceUnlockEnrolResult, DaemonError> {
    request.validate().map_err(DaemonError::InvalidParams)?;
    let (device, volume_uuid) = current_volume().map_err(|_| refusal())?;
    let key = private_key_file(&request.passphrase).map_err(|_| refusal())?;
    let fd = key.as_raw_fd();
    let mut command = Command::new(HELPER);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin")
        .env("LANG", "C.UTF-8");
    command
        .arg("enrol")
        .arg(&device)
        .arg("--key-file")
        .arg(format!("/proc/self/fd/{fd}"))
        .arg("--session-uid")
        .arg(uid.to_string());
    if !request.allow_unsealed {
        command.arg("--require-tpm");
    }
    for network in &request.networks {
        command.arg(format!("--network={network}"));
    }
    inherit_private_key(&mut command, &key);
    let result = run_bounded(
        command,
        // The existing registrar can wait 120s for each reply segment.
        // Let its ordinary timeout return and roll back before our outer bound.
        Duration::from_secs(360),
        65536,
        std::io::sink(),
        std::io::sink(),
    )
    .map_err(|_| refusal())?;
    drop(key);
    if result != Ended::Exited(0) {
        return Err(refusal());
    }
    let output = Output::default();
    let mut command = Command::new(HELPER);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin")
        .env("LANG", "C.UTF-8");
    command.arg("status").arg(device);
    if run_bounded(
        command,
        Duration::from_secs(15),
        65536,
        output.clone(),
        std::io::sink(),
    )
    .map_err(|_| refusal())?
        != Ended::Exited(0)
    {
        return Err(refusal());
    }
    let bytes = output.0.lock().map_err(|_| refusal())?;
    let status: Status = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
    drop(bytes);
    Ok(DeviceUnlockEnrolResult {
        volume_uuid,
        keyslot: status.keyslot,
        sealing: status.sealing,
        networks: status.networks,
        siblings: status.siblings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn enrollment_secret_is_sealed_and_not_an_argument() {
        let mut file = private_key_file("private-fixture-answer").unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "private-fixture-answer");
        assert!(file.write_all(b"replacement").is_err());
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn enrollment_sealed_descriptor_reaches_the_real_bounded_child_without_secret_argv() {
        let key = private_key_file("private-fixture-answer").unwrap();
        let mut command = Command::new("/usr/bin/cat");
        command.arg(format!("/proc/self/fd/{}", key.as_raw_fd()));
        assert!(command.get_args().all(|argument| {
            !argument
                .to_string_lossy()
                .contains("private-fixture-answer")
        }));
        inherit_private_key(&mut command, &key);
        let output = Output::default();
        assert_eq!(
            run_bounded(
                command,
                Duration::from_secs(5),
                4096,
                output.clone(),
                std::io::sink()
            )
            .unwrap(),
            Ended::Exited(0)
        );
        assert_eq!(&*output.0.lock().unwrap(), b"private-fixture-answer");
    }

    #[test]
    fn btrfs_enrollment_requires_one_registered_and_present_mapper() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = root.path().join("d460ffbd-45e9-40f8-b5c6-6f468263df34");
        std::fs::create_dir_all(filesystem.join("devices/dm-0")).unwrap();
        std::fs::create_dir_all(filesystem.join("devinfo/1")).unwrap();
        std::fs::write(filesystem.join("devices/dm-0/dev"), "253:0\n").unwrap();
        assert!(single_btrfs_device(root.path(), "253:0").unwrap());
        assert!(!single_btrfs_device(root.path(), "253:9").unwrap());
        // Even an absent second device keeps the root ineligible.
        std::fs::create_dir_all(filesystem.join("devinfo/2")).unwrap();
        assert!(!single_btrfs_device(root.path(), "253:0").unwrap());
        std::fs::remove_dir(filesystem.join("devinfo/2")).unwrap();
        std::fs::remove_file(filesystem.join("devices/dm-0/dev")).unwrap();
        assert!(single_btrfs_device(root.path(), "253:0").is_err());
        let ordinary = RootMount {
            number: "1:3",
            filesystem: "ext4",
            source: "/dev/null",
        };
        assert!(mounted_mapper_number(&ordinary, root.path()).is_err());
    }

    #[test]
    fn encrypted_root_uses_bootc_sysroot_instead_of_container_overlay() {
        let mounts = "1 0 0:42 / / rw - overlay overlay rw\n2 1 253:0 / /sysroot rw - ext4 /dev/mapper/root rw\n";
        assert_eq!(root_mount(mounts).unwrap().number, "253:0");
        let mounts = "1 0 0:42 / / rw - overlay overlay rw\n2 1 0:77 /root /sysroot rw - btrfs /dev/mapper/root rw\n";
        let root = root_mount(mounts).unwrap();
        assert_eq!(root.number, "0:77");
        assert_eq!(root.source, "/dev/mapper/root");
        assert_eq!(root.filesystem, "btrfs");
    }
}

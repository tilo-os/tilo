//! Mount a just-written installed root at an explicitly authorized device boundary.
//! The caller owns target authorization, the private mountpoint and the copy policy.
//! Missing or untyped partitions fail closed; only a complete root-free listing
//! returns `false`. Every subprocess has an output cap and a deadline.

use std::io;
use std::path::Path;

/// Visit an installed root after the caller has authorized and written the device.
///
/// # Errors
/// Returns an error for unavailable or incomplete partition discovery, failed
/// mounts, or the caller's copy failure. Returns `false` only for a root-free
/// listing or a failed loop attachment; the caller must verify image kind
/// before using an installer-medium fallback.
#[cfg(target_os = "linux")]
pub fn with_written_root(
    device: &Path,
    mountpoint: &Path,
    copy: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<bool> {
    with_written_root_observed(device, mountpoint, copy, |_| {})
}

/// Visit the root while recording the actual loop attachment outcome.
/// The observer runs after the bounded losetup attempt, even when it fails
/// to start; direct block-device roots do not need a loop and emit no event.
#[cfg(target_os = "linux")]
pub fn with_written_root_observed(
    device: &Path,
    mountpoint: &Path,
    copy: impl FnOnce(&Path) -> io::Result<()>,
    observed: impl FnOnce(bool),
) -> io::Result<bool> {
    // A real target disk already has partition nodes. A file image is not a
    // block device: `lsblk` cannot see its partitions, and treating that
    // failure as fatal would skip the loop mount that exposes them.
    let block_device = std::fs::metadata(device).is_ok_and(|meta| {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_block_device()
    });
    if block_device {
        // The image was just written over this disk. The kernel can still be
        // serving the previous partition table. Updating in place does not add
        // partitions it has never seen, so drop the old set and add the one
        // now on the disk. Otherwise the Tilo root is invisible and the
        // handoff is stored where a boot never reads it. Never drop the
        // kernel's partitions while a filesystem is mounted, or when the
        // mount check itself failed.
        let mount_state = Tool::new("lsblk")
            .args(["-ln", "-o", "MOUNTPOINT"])
            .arg(device)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| mountpoint_listing_is_busy(&String::from_utf8_lossy(&output.stdout)));
        if mount_state == Some(false) {
            let _ = Tool::new("partx").args(["-d"]).arg(device).status();
            if let Some(device_path) = device.to_str() {
                publish_partition_table(device_path);
            }
        }
    }
    let mut nodes_never_appeared = false;
    if block_device && let Some(device_path) = device.to_str() {
        match find_tilo_root_partition(device_path) {
            RootProbe::Found(root_part) => {
                return with_mounted_root(&root_part, mountpoint, copy).map(|()| true);
            }
            RootProbe::Unavailable => {
                return Err(handoff_not_applied());
            }
            // The kernel never published a partition. Try a loop device
            // before giving up; slack is not a substitute for a root that
            // has not appeared yet.
            RootProbe::Unpublished => nodes_never_appeared = true,
            RootProbe::Absent => {}
        }
    }
    let loop_dev = {
        let attachment = Tool::new("losetup")
            .args(["-fP", "--show"])
            .arg(device)
            .output();
        observed(
            attachment
                .as_ref()
                .is_ok_and(|output| output.status.success()),
        );
        let output = match attachment {
            Ok(output) if output.status.success() => output,
            _ => {
                if nodes_never_appeared {
                    return Err(handoff_not_applied());
                }
                return Ok(false);
            }
        };
        String::from_utf8(output.stdout)
            .map_err(io::Error::other)?
            .trim()
            .to_string()
    };
    let root_part = find_tilo_root_partition(&loop_dev);
    let result = match root_part {
        RootProbe::Found(root_part) => {
            with_mounted_root(&root_part, mountpoint, copy).map(|()| true)
        }
        // Children were visible and none is a Tilo root: an installer stick.
        // Its slack is what the live installer reads.
        RootProbe::Absent => Ok(false),
        RootProbe::Unpublished | RootProbe::Unavailable => Err(handoff_not_applied()),
    };
    let _ = Tool::new("losetup").args(["-d", &loop_dev]).status();
    result
}

/// Ask the kernel for `device`'s partition table until a node appears.
///
/// A single `partx` right after the image is attached often reports that it
/// could not read the table. The installed root is then missing and the
/// Wi-Fi profile and device key are not copied.
#[cfg(target_os = "linux")]
fn publish_partition_table(device: &str) {
    for _ in 0..20 {
        let _ = Tool::new("partx").args(["-a", device]).status();
        if Path::new(&format!("{device}1")).exists() || Path::new(&format!("{device}p1")).exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = Tool::new("blockdev").args(["--rereadpt", device]).status();
}

/// Copy onto a root that is already part of the written disk.
///
/// Failure stays a failure. Slack is only for a live installer medium, which
/// has no installed root; a full-disk boot never reads that slack.
#[cfg(target_os = "linux")]
pub fn with_mounted_root(
    root_part: &str,
    mountpoint: &Path,
    copy: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mount = Tool::new("mount")
        .arg(root_part)
        .arg(mountpoint)
        .status()
        .map_err(|error| {
            io::Error::other(format!(
                "could not mount the installed root {root_part}: {error}"
            ))
        })?;
    if !mount.success() {
        return Err(io::Error::other(format!(
            "could not mount the installed root {root_part}"
        )));
    }
    let _umount = UnmountOnDrop(mountpoint);
    // bootc leaves the installed root mounted read-only. A cold mount can
    // come up the same way; the handoff has to be able to create files.
    // Remount the mountpoint, not the device, so a second mount of that
    // device is not the one that changes.
    let remount = Tool::new("mount")
        .args(["-o", "remount,rw"])
        .arg(mountpoint)
        .status()
        .map_err(|error| {
            io::Error::other(format!(
                "could not remount the installed root {root_part} read-write: {error}"
            ))
        })?;
    if !remount.success() {
        return Err(io::Error::other(format!(
            "could not remount the installed root {root_part} read-write"
        )));
    }
    copy(mountpoint)
}

/// The mount has to go away on every exit, including a remount that fails
/// before the copy.
#[cfg(target_os = "linux")]
struct UnmountOnDrop<'a>(&'a Path);

#[cfg(target_os = "linux")]
impl Drop for UnmountOnDrop<'_> {
    fn drop(&mut self) {
        let _ = Tool::new("umount").arg(self.0).status();
    }
}

pub fn mountpoint_listing_is_busy(lsblk_mountpoints: &str) -> bool {
    lsblk_mountpoints
        .lines()
        .any(|line| !line.trim().is_empty())
}

#[cfg(target_os = "linux")]
enum RootProbe {
    Found(String),
    /// Partition nodes were listed and none is a Tilo root. An installer
    /// stick looks like this; its handoff belongs in the slack.
    Absent,
    /// The listing stayed empty for the whole wait. The nodes may still
    /// appear, so this is not an installer stick.
    Unpublished,
    /// `lsblk` could not be run, so an installed root may still be there.
    Unavailable,
}

#[cfg(target_os = "linux")]
fn handoff_not_applied() -> io::Error {
    io::Error::other(
        "could not inspect the written disk for an installed root, so Wi-Fi and identity were not applied",
    )
}

#[cfg(target_os = "linux")]
fn find_tilo_root_partition(loop_dev: &str) -> RootProbe {
    // Partition nodes appear before blkid fills in the type. A failed probe
    // is not "no root": embedding the handoff then reports success while the
    // installed system never reads it. A card that was just overwritten can
    // take several seconds to publish those nodes. Giving up on an empty
    // listing after the first half-second stores Wi-Fi and identity where
    // the boot never reads them.
    const PROBES: u32 = 40;
    for attempt in 1..=PROBES {
        let output = Tool::new("lsblk")
            .args(["-ln", "-o", "PATH,FSTYPE,LABEL"])
            .arg(loop_dev)
            .output()
            .ok()
            .filter(|output| output.status.success());
        let Some(output) = output else {
            if attempt == PROBES {
                return RootProbe::Unavailable;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(part) = select_tilo_root_partition(loop_dev, &text) {
            return RootProbe::Found(part);
        }
        // A partition node with no filesystem type is not "no root". blkid
        // fills the type in after the node appears. Treating that listing as
        // absent stores Wi-Fi and identity in slack the booted system never
        // reads, and the write is reported successful.
        if root_listing_waits_for_filesystem_type(loop_dev, &text) {
            if attempt == PROBES {
                return RootProbe::Unavailable;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        let has_child = text.lines().any(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|path| is_partition_of(loop_dev, path))
        });
        // Typed partitions and no Tilo root is a finished listing: the
        // installer stick. An empty listing is not. The nodes can show up
        // seconds after the table is reread, and treating that as "no root"
        // stores the handoff where a boot never reads it.
        if has_child {
            return RootProbe::Absent;
        }
        if attempt == PROBES {
            return RootProbe::Unpublished;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    RootProbe::Unavailable
}

/// `loop1` is a prefix of `loop10`. A partition of this disk is `loop1p1`,
/// not another disk whose name merely continues with a digit.
fn is_partition_of(device: &str, path: &str) -> bool {
    let Some(rest) = path.strip_prefix(device) else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    if device.ends_with(|byte: char| byte.is_ascii_digit()) {
        !rest.starts_with(|byte: char| byte.is_ascii_digit())
    } else {
        true
    }
}

/// A child partition is listed, but blkid has not reported its type yet.
pub fn root_listing_waits_for_filesystem_type(device: &str, lsblk_text: &str) -> bool {
    lsblk_text.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let Some(path) = parts.next() else {
            return false;
        };
        is_partition_of(device, path) && parts.next().is_none()
    })
}

/// Prefer a btrfs child. Tilo's root is btrfs; an earlier ext4 boot partition
/// is not the installed system. Landing Wi-Fi and the device key there leaves
/// the booted machine without them. An ext4 or xfs child is the root only
/// when the disk has no btrfs. A single filesystem on the disk itself is last.
pub fn select_tilo_root_partition(loop_dev: &str, lsblk_text: &str) -> Option<String> {
    let mut btrfs_child = None;
    let mut other_child = None;
    let mut whole = None;
    for line in lsblk_text.lines() {
        let mut parts = line.split_whitespace();
        let Some(path) = parts.next() else {
            continue;
        };
        let Some(fstype) = parts.next() else {
            continue;
        };
        if !matches!(fstype, "btrfs" | "ext4" | "xfs") {
            continue;
        }
        // The live installer's boot partition is ext4 and labeled TILO_BOOT.
        // It is not the installed system. Writing the handoff there means the
        // installer never sees it in the slack, and a direct boot never sees
        // it on the root.
        if matches!(fstype, "ext4" | "xfs") && parts.next() == Some("TILO_BOOT") {
            continue;
        }
        if path != loop_dev && !is_partition_of(loop_dev, path) {
            continue;
        }
        if path == loop_dev {
            if whole.is_none() {
                whole = Some(path.to_string());
            }
            continue;
        }
        if fstype == "btrfs" {
            if btrfs_child.is_none() {
                btrfs_child = Some(path.to_string());
            }
        } else if other_child.is_none() {
            other_child = Some(path.to_string());
        }
    }
    btrfs_child.or(other_child).or(whole)
}

/// # Errors
/// Always returns `Unsupported` on this platform.
#[cfg(not(target_os = "linux"))]
pub fn with_written_root(
    _device: &Path,
    _mountpoint: &Path,
    _copy: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mounting a written disk requires Linux",
    ))
}

/// # Errors
/// Always returns `Unsupported` on this platform.
#[cfg(not(target_os = "linux"))]
pub fn with_mounted_root(
    _root_part: &str,
    _mountpoint: &Path,
    _copy: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mounting an installed root requires Linux",
    ))
}

#[cfg(target_os = "linux")]
struct Tool {
    program: String,
    args: Vec<std::ffi::OsString>,
}

#[cfg(target_os = "linux")]
impl Tool {
    fn new(name: &str) -> Self {
        Self {
            program: crate::system_tool(name),
            args: Vec::new(),
        }
    }
    fn args(&mut self, args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>) -> &mut Self {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }
    fn arg(&mut self, arg: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }
    fn output(&self) -> io::Result<bounded_command::Output> {
        bounded_command::run_os(
            &self.program,
            &self.args,
            None,
            1024 * 1024,
            std::time::Duration::from_secs(30),
        )
        .map_err(io::Error::other)
    }
    fn status(&self) -> io::Result<std::process::ExitStatus> {
        self.output().map(|out| out.status)
    }
}

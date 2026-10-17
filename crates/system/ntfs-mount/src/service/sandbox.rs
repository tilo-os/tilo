//! Starting a helper that reads a hostile volume: the preflight checks and
//! the userspace NTFS driver.
//!
//! Between `fork` and `exec` the child, still root, moves into its own
//! mount, network, IPC and UTS namespaces, so it has no network at all. In
//! its private mount namespace a small tmpfs replaces the sandbox directory;
//! the one block device it may use appears there as a node owned by the
//! helper account (the driver opens devices by path, and a node it owns is
//! the only one it can open). It then sets `no_new_privs`, restricts itself
//! with Landlock to the program files, that directory, `/dev/fuse` and
//! `/dev/null`, drops to the dedicated helper account, and installs the
//! foreign-driver seccomp filter: no sockets, no pidfds, signals only to
//! itself. Whatever a crafted volume achieves inside the driver stays there.
//!
//! The service is single-threaded, so the child may allocate before `exec`.

use std::ffi::{CString, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use sandbox_core::landlock::{self, FsRule, NetRestriction, RulesetSpec};
use sandbox_core::seccomp;

/// Where a helper given [`Device::Fd`] finds the volume.
pub const DEVICE_FD: RawFd = 3;
/// The volume's node inside the sandbox directory.
pub const NODE: &str = "volume";
/// A directory the helper owns, inside the sandbox directory, for a driver
/// that insists on checking its mount point.
pub const MOUNT_DIR: &str = "mnt";

/// How the helper reaches the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// As descriptor [`DEVICE_FD`], read-only.
    Fd,
    /// As the node [`NODE`] in the sandbox directory, for a program that
    /// opens its device by path.
    Node { writable: bool },
}

/// The dedicated account helpers run as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Account {
    pub uid: u32,
    pub gid: u32,
}

/// One confined helper.
pub struct Confinement<'a> {
    pub sandbox_dir: &'a Path,
    pub account: Account,
    /// The service's own descriptor for the volume, already checked to be
    /// the device the request named.
    pub device: &'a File,
    pub access: Device,
    /// Further descriptors to hand over: (the service's, the helper's).
    pub inherit: Vec<(RawFd, RawFd)>,
    /// Extra read-only paths (the handoff library).
    pub read_paths: Vec<PathBuf>,
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::other("path holds a NUL"))
}

/// Everything the child needs, prepared before `fork`.
struct Plan {
    sandbox_dir: CString,
    node: CString,
    mount_dir: CString,
    tmpfs_options: CString,
    rdev: libc::dev_t,
    access: Device,
    device_fd: RawFd,
    inherit: Vec<(RawFd, RawFd)>,
    account: Account,
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
}

impl Plan {
    fn namespaces(&self) -> io::Result<()> {
        // SAFETY: plain syscalls on prepared, NUL-terminated strings.
        unsafe {
            libc::setsid();
            check(libc::unshare(
                libc::CLONE_NEWNS | libc::CLONE_NEWNET | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS,
            ))?;
            check(libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))?;
        }
        if let Device::Node { writable } = self.access {
            let mode = if writable { 0o600 } else { 0o400 };
            // SAFETY: as above; every path was prepared before fork.
            unsafe {
                check(libc::mount(
                    c"tilo-ntfs-sandbox".as_ptr(),
                    self.sandbox_dir.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NOEXEC,
                    self.tmpfs_options.as_ptr().cast(),
                ))?;
                check(libc::mknod(
                    self.node.as_ptr(),
                    libc::S_IFBLK | mode,
                    self.rdev,
                ))?;
                check(libc::chown(
                    self.node.as_ptr(),
                    self.account.uid,
                    self.account.gid,
                ))?;
                check(libc::mkdir(self.mount_dir.as_ptr(), 0o700))?;
                check(libc::chown(
                    self.mount_dir.as_ptr(),
                    self.account.uid,
                    self.account.gid,
                ))?;
            }
        }
        Ok(())
    }

    fn descriptors(&self) -> io::Result<()> {
        // Park every source above the targets first, so no target number
        // overwrites a source that is still to be moved.
        let mut moves = Vec::with_capacity(self.inherit.len().saturating_add(1));
        if self.access == Device::Fd {
            moves.push((self.device_fd, DEVICE_FD));
        }
        moves.extend(self.inherit.iter().copied());
        let mut parked = Vec::with_capacity(moves.len());
        for (source, target) in moves {
            // SAFETY: duplicating a descriptor this process holds.
            let high = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 64) };
            check(high)?;
            parked.push((high, target));
        }
        let targets: Vec<RawFd> = parked.iter().map(|(_, target)| *target).collect();
        for (high, target) in parked {
            // SAFETY: as above; dup2 clears close-on-exec on the target.
            unsafe {
                check(libc::dup2(high, target))?;
                libc::close(high);
            }
        }
        // Nothing else the service holds (its listening socket, another
        // drive's descriptors) may cross into the helper.
        let last = targets.iter().copied().max().unwrap_or(2);
        for fd in 3..=last {
            if !targets.contains(&fd) {
                // SAFETY: setting a flag on a descriptor number; EBADF for
                // one that is not open is harmless.
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            }
        }
        let first = libc::c_uint::try_from(last.saturating_add(1)).map_err(io::Error::other)?;
        // SAFETY: marks a descriptor range close-on-exec; no memory involved.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                first,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if marked != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn restrict(&self) -> io::Result<()> {
        // SAFETY: prctl with constant arguments.
        check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
        let rules: Vec<FsRule> =
            landlock::access_plan(&self.read_paths, &self.write_paths, landlock::FS_ABI);
        let status = landlock::restrict_self(
            &RulesetSpec::application(rules, NetRestriction::DenyAll),
            |_, _| {},
        )
        .map_err(io::Error::other)?;
        if landlock::outcome_from_status(&status.ruleset)
            != landlock::LandlockOutcome::FullyEnforced
        {
            return Err(io::Error::other("Landlock is not fully enforced"));
        }
        let Account { uid, gid } = self.account;
        // SAFETY: credential syscalls with plain integers.
        unsafe {
            check(libc::setgroups(0, std::ptr::null()))?;
            check(libc::setresgid(gid, gid, gid))?;
            check(libc::setresuid(uid, uid, uid))?;
        }
        // SAFETY: getpid cannot fail.
        let own = u32::try_from(unsafe { libc::getpid() }).map_err(io::Error::other)?;
        for program in [
            seccomp::clone3_compat_program(),
            seccomp::driver_program(own),
        ] {
            seccomp::install(&program.map_err(io::Error::other)?).map_err(io::Error::other)?;
        }
        Ok(())
    }
}

/// Start `program` with `args`, confined as `confinement` describes.
///
/// # Errors
///
/// Returns the I/O error if the device cannot be inspected, the child
/// cannot be confined, or the program cannot be started.
pub fn spawn(
    confinement: &Confinement<'_>,
    program: &Path,
    args: &[OsString],
    env: &[(&str, OsString)],
) -> io::Result<Child> {
    let rdev = confinement.device.metadata()?.rdev();
    let dir = confinement.sandbox_dir;
    let mut read_paths: Vec<PathBuf> = ["/usr", "/etc", "/proc/self", "/proc/filesystems"]
        .iter()
        .map(PathBuf::from)
        .collect();
    read_paths.extend(confinement.read_paths.iter().cloned());
    let mut write_paths = vec![PathBuf::from("/dev/null")];
    if matches!(confinement.access, Device::Node { .. }) {
        write_paths.push(dir.to_path_buf());
        write_paths.push(PathBuf::from("/dev/fuse"));
    }
    let plan = Plan {
        sandbox_dir: c_path(dir)?,
        node: c_path(&dir.join(NODE))?,
        mount_dir: c_path(&dir.join(MOUNT_DIR))?,
        tmpfs_options: CString::new(format!(
            "mode=0700,uid={},gid={},size=64k,nr_inodes=16",
            confinement.account.uid, confinement.account.gid
        ))
        .map_err(io::Error::other)?,
        rdev,
        access: confinement.access,
        device_fd: confinement.device.as_raw_fd(),
        inherit: confinement.inherit.clone(),
        account: confinement.account,
        read_paths,
        write_paths,
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("LANG", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    // SAFETY: the closure runs in the child between fork and exec. The
    // service is single-threaded, so no other thread can hold a lock the
    // child inherits.
    unsafe {
        command.pre_exec(move || {
            plan.namespaces()?;
            plan.descriptors()?;
            plan.restrict()
        });
    }
    command.spawn()
}

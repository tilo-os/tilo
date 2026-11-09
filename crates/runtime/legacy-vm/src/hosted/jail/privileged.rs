//! What `tilo-hosted-jail` does as root. It runs single-threaded, so it
//! forks freely.
//!
//! Every input is the service's: the slot number (bounded by the
//! configuration), the VM's size, and three directory descriptors the
//! service opened itself. Paths come only from the root-owned
//! configuration.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{GUEST_DIR, GUEST_FD, JailConfig, SOCKETS_DIR, SOCKETS_FD, VMM_DIR, VMM_FD};
use crate::hosted::network::{self, BRIDGE, TAP, UPLINK};

/// What `start` needs besides the configuration.
pub struct Start {
    pub slot: u32,
    pub vcpus: u32,
    pub memory_mib: u32,
    /// Boot from `/vm/firecracker.json`; else wait for a snapshot.
    pub boot: bool,
}

fn os(what: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{what}: {error}"))
}

/// Say which step an error comes from.
trait Step<T> {
    fn step(self, what: &str) -> io::Result<T>;
}

impl<T, E: Into<io::Error>> Step<T> for Result<T, E> {
    fn step(self, what: &str) -> io::Result<T> {
        self.map_err(|error| {
            let error: io::Error = error.into();
            io::Error::new(error.kind(), format!("{what}: {error}"))
        })
    }
}

fn check(result: libc::c_int, what: &str) -> io::Result<()> {
    if result < 0 { Err(os(what)) } else { Ok(()) }
}

/// Refuse every caller but the service, then become root for the rest.
///
/// The helper's file capabilities only let it change its ids: a process
/// that becomes uid 0 keeps the capabilities it had, and is given root's
/// only by `execve`. So the service's call switches to root and runs the
/// helper again, with the same arguments and descriptors; that run, whose
/// caller is root, goes on. Only root can start the helper as root.
pub fn enter(config: &JailConfig) -> io::Result<()> {
    let caller = rustix::process::getuid().as_raw();
    if caller == 0 && rustix::process::geteuid().as_raw() == 0 {
        return Ok(());
    }
    if caller != config.service_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("only uid {} may use this helper", config.service_uid),
        ));
    }
    // SAFETY: plain system calls; with the helper's capabilities they make
    // every id root's.
    unsafe {
        check(libc::setgroups(0, std::ptr::null()), "setgroups")?;
        check(libc::setresgid(0, 0, 0), "becoming root (setresgid)")?;
        check(libc::setresuid(0, 0, 0), "becoming root (setresuid)")?;
    }
    let error = std::process::Command::new("/proc/self/exe")
        .args(std::env::args_os().skip(1))
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .exec();
    Err(io::Error::new(
        error.kind(),
        format!("running the helper as root: {error}"),
    ))
}

/// A directory descriptor the service passed, checked: a directory, owned
/// by the service or (left by a crash) by the slot's VM.
fn passed_dir(fd: RawFd, owners: &[u32], what: &str) -> io::Result<File> {
    // SAFETY: the service passed `fd` open for this process; nothing else
    // here owns it.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file
        .metadata()
        .map_err(|e| io::Error::new(e.kind(), format!("the {what} directory: {e}")))?;
    if !meta.is_dir() || (!owners.is_empty() && !owners.contains(&meta.uid())) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the {what} directory is not the service's"),
        ));
    }
    // SAFETY: `fcntl` on a descriptor this owns.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok(file)
}

/// Make `dir` and the regular files directly in it `uid`'s, group `gid`,
/// readable and writable by both; the directory set-group-id so what is
/// made in it is `gid`'s too. Symbolic links and anything else are left
/// alone, and never followed.
fn hand_over(dir: &File, uid: u32, gid: u32) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags};
    let fd = dir.as_raw_fd();
    let entries = rustix::fs::Dir::read_from(dir).map_err(io::Error::from)?;
    for entry in entries {
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let Ok(file) = rustix::fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            continue;
        };
        let stat = rustix::fs::fstat(&file).map_err(io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            continue;
        }
        rustix::fs::fchown(
            &file,
            Some(rustix::fs::Uid::from_raw(uid)),
            Some(rustix::fs::Gid::from_raw(gid)),
        )
        .map_err(io::Error::from)?;
        let mode = (stat.st_mode & 0o777) | 0o660;
        rustix::fs::fchmod(&file, Mode::from_raw_mode(mode)).map_err(io::Error::from)?;
    }
    // SAFETY: `fchown`/`fchmod` on a directory descriptor this owns.
    unsafe {
        check(libc::fchown(fd, uid, gid), "handing a directory over")?;
        check(libc::fchmod(fd, 0o2770), "handing a directory over")?;
    }
    Ok(())
}

fn fresh_dir(path: &Path, mode: u32, owner: Option<u32>) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    if let Some(owner) = owner {
        std::os::unix::fs::chown(path, Some(owner), Some(owner))?;
    }
    Ok(())
}

/// `/sys/fs/cgroup/<cgroup>`, with the controllers the jailer sets.
fn cgroup_parent(config: &JailConfig) -> io::Result<()> {
    let dir = config.cgroup_dir();
    std::fs::create_dir_all(&dir)?;
    // Each controller on its own: one the kernel lacks must not keep the
    // others off.
    for controller in ["+cpu", "+memory", "+pids"] {
        let _ = std::fs::write(dir.join("cgroup.subtree_control"), controller);
    }
    Ok(())
}

fn net_cgroup(config: &JailConfig, slot: u32) -> std::path::PathBuf {
    config
        .cgroup_dir()
        .join(format!("{}-net", JailConfig::id(slot)))
}

fn vm_cgroup(config: &JailConfig, slot: u32) -> std::path::PathBuf {
    config.cgroup_dir().join(JailConfig::id(slot))
}

/// Set the slot's VM up and become its Firecracker. Returns only on
/// failure.
pub fn start(config: &JailConfig, start: &Start) -> io::Error {
    match prepare(config, start) {
        Ok(mut jailer) => {
            let error = jailer.exec();
            io::Error::new(error.kind(), format!("starting the jailer: {error}"))
        }
        Err(error) => error,
    }
}

fn prepare(config: &JailConfig, start: &Start) -> io::Result<std::process::Command> {
    let slot = start.slot;
    let (vm, service_gid) = (config.vm_uid(slot), config.service_gid);
    let owners = [config.service_uid, vm];
    let vmm = passed_dir(VMM_FD, &owners, "VM's state")?;
    let sockets = passed_dir(SOCKETS_FD, &owners, "VM's sockets")?;
    let guest = passed_dir(GUEST_FD, &[], "guest kernel's")?;
    hand_over(&vmm, vm, service_gid).step("handing the VM its state")?;
    hand_over(&sockets, vm, service_gid).step("handing the VM its sockets")?;

    let jail = config.jail_dir(slot);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o711)
        .create(&config.chroot_base)
        .step("making the jails' directory")?;
    if let Some(exec_dir) = jail.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o711)
            .create(exec_dir)?;
    }
    fresh_dir(&jail, 0o700, None).step("making the jail")?;
    let root = jail.join("root");
    for point in [VMM_DIR, SOCKETS_DIR, GUEST_DIR] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root.join(point.trim_start_matches('/')))?;
    }
    cgroup_parent(config).step("making the VMs' cgroup")?;
    if let Some(net) = &config.network {
        connect(config, net, slot)?;
    }

    // The jail's directories, in a mount namespace of this process's: the
    // jailer copies it into its own and pivots into the chroot.
    // SAFETY: plain system calls on constant arguments and paths built here.
    unsafe {
        check(libc::unshare(libc::CLONE_NEWNS), "a mount namespace")?;
        check(
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ),
            "making mounts private",
        )?;
    }
    for (fd, point, read_only) in [
        (&vmm, VMM_DIR, false),
        (&sockets, SOCKETS_DIR, false),
        (&guest, GUEST_DIR, true),
    ] {
        bind(fd, &root.join(point.trim_start_matches('/')), read_only)?;
    }
    // Sockets and snapshot files are the service's group's to use.
    // SAFETY: `umask` has no preconditions.
    unsafe { libc::umask(0o007) };

    // The guest's memory and a quarter more for Firecracker and the page
    // cache of a restored VM's memory file, plus a fixed margin.
    let guest = u64::from(start.memory_mib).saturating_mul(1 << 20);
    let memory = guest.saturating_add(guest / 4).saturating_add(256 << 20);
    let mut jailer = std::process::Command::new(&config.jailer);
    jailer
        .env_clear()
        .arg("--id")
        .arg(JailConfig::id(slot))
        .arg("--exec-file")
        .arg(&config.firecracker)
        .arg("--uid")
        .arg(vm.to_string())
        .arg("--gid")
        .arg(vm.to_string())
        .arg("--chroot-base-dir")
        .arg(&config.chroot_base)
        .arg("--cgroup-version")
        .arg("2")
        .arg("--parent-cgroup")
        .arg(&config.cgroup)
        .arg("--cgroup")
        .arg(format!("memory.max={memory}"))
        .arg("--cgroup")
        .arg(format!(
            "cpu.max={} 100000",
            u64::from(start.vcpus.max(1)) * 100_000
        ))
        .arg("--cgroup")
        .arg(format!("pids.max={}", start.vcpus.saturating_add(64)))
        .arg("--resource-limit")
        .arg("no-file=4096")
        .arg("--")
        .arg("--api-sock")
        .arg(format!("{SOCKETS_DIR}/api.sock"));
    if start.boot {
        jailer
            .arg("--config-file")
            .arg(format!("{VMM_DIR}/firecracker.json"));
    }
    Ok(jailer)
}

/// Bind the directory `source` names at `target`. A descriptor opened
/// before this process made its own mount namespace names a mount of the
/// old one, which `mount` refuses, so the bind goes by the directory's
/// path, and the result must be the very directory the service opened: a
/// directory swapped in between is refused.
fn bind(source: &File, target: &Path, read_only: bool) -> io::Result<()> {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", source.as_raw_fd()))?;
    let from = CString::new(path.as_os_str().as_bytes())?;
    let to = CString::new(target.as_os_str().as_bytes())?;
    // SAFETY: both strings live across the calls.
    unsafe {
        check(
            libc::mount(
                from.as_ptr(),
                to.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            ),
            "binding a directory into the jail",
        )?;
    }
    let (passed, bound) = (source.metadata()?, std::fs::metadata(target)?);
    if (passed.dev(), passed.ino()) != (bound.dev(), bound.ino()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} changed while it was bound", path.display()),
        ));
    }
    if read_only {
        // SAFETY: as above.
        unsafe {
            check(
                libc::mount(
                    std::ptr::null(),
                    to.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND
                        | libc::MS_REMOUNT
                        | libc::MS_RDONLY
                        | libc::MS_NOSUID
                        | libc::MS_NODEV,
                    std::ptr::null(),
                ),
                "making the guest directory read-only",
            )?;
        }
    }
    Ok(())
}

/// The VM's network ([`network`]): a namespace owned by the slot's network
/// uid, holding the VM's tap and `pasta`'s, bridged and filtered. Leaves
/// this process in that namespace, so the jailer and Firecracker are.
fn connect(config: &JailConfig, net: &super::NetworkConfig, slot: u32) -> io::Result<()> {
    let (vm, net_uid) = (config.vm_uid(slot), config.net_uid(slot));
    let net_dir = config.net_dir(slot);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o711)
        .create(config.chroot_base.join("net"))?;
    fresh_dir(&net_dir, 0o700, Some(net_uid)).step("making the network's directory")?;
    let host = File::open("/proc/self/ns/net")?;
    let holder = spawn_holder(net_uid)?;
    let cgroup = net_cgroup(config, slot);
    let _ = std::fs::create_dir(&cgroup);
    std::fs::write(cgroup.join("cgroup.procs"), holder.to_string())
        .step("moving the network into its cgroup")?;

    let namespace = File::open(format!("/proc/{holder}/ns/net"))?;
    // SAFETY: `setns` on a namespace descriptor this owns.
    check(
        unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNET) },
        "entering the VM's network namespace",
    )?;
    for sysctl in ["all", "default"] {
        let path = format!("/proc/sys/net/ipv6/conf/{sysctl}/disable_ipv6");
        if Path::new(&path).exists() {
            std::fs::write(path, "1").step("turning IPv6 off")?;
        }
    }
    create_tap(TAP, vm)?;
    let control = Control::open()?;
    control.add_bridge(BRIDGE)?;
    control.enslave(BRIDGE, TAP)?;
    // The filter is in before anything is connected: until it is, the
    // namespace has no route out at all.
    let netfilter = {
        // SAFETY: a new socket nothing else owns.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                tilo_egress::nft::Netfilter::PROTOCOL,
            )
        };
        check(fd, "a netfilter socket")?;
        // SAFETY: as above.
        unsafe { OwnedFd::from_raw_fd(fd) }
    };
    let blocked = config.blocked().map_err(io::Error::other)?;
    tilo_egress::nft::Netfilter::new(netfilter)?
        .install_guest(&network::guest_filter(&blocked))
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "installing the VM's network filter ({e}); the kernel needs \
                     nf_tables with bridge support (CONFIG_NF_TABLES_BRIDGE)"
                ),
            )
        })?;

    let pid_file = net_dir.join("pasta.pid");
    let pasta = spawn_pasta(&net.pasta, &host, net_uid, holder, &pid_file, net.resolver)?;
    std::fs::write(cgroup.join("cgroup.procs"), pasta.to_string())
        .step("moving pasta into its cgroup")?;
    control.enslave(BRIDGE, UPLINK)?;
    for link in ["lo", TAP, UPLINK, BRIDGE] {
        control.up(link)?;
    }
    Ok(())
}

/// A process of the network uid's, holding a user namespace it owns and a
/// network namespace in it, until killed. It keeps no capability outside
/// that namespace and dies with this process's successor (Firecracker).
fn spawn_holder(net_uid: u32) -> io::Result<u32> {
    let (ready_r, ready_w) = pipe()?;
    // SAFETY: this process is single-threaded; the child only makes system
    // calls and writes, then loops in `pause`.
    let pid = unsafe { libc::fork() };
    check(pid, "fork")?;
    if pid == 0 {
        drop(ready_r);
        // It outlives everything here: it keeps no descriptor of the
        // helper's (the VM's directories among them) but its status pipe.
        let keep = ready_w.as_raw_fd().unsigned_abs();
        // SAFETY: closing descriptors only.
        unsafe {
            libc::syscall(libc::SYS_close_range, 0_u32, keep.saturating_sub(1), 0_u32);
            libc::syscall(
                libc::SYS_close_range,
                keep.saturating_add(1),
                u32::MAX,
                0_u32,
            );
        }
        let outcome = holder_child(net_uid);
        let mut ready = File::from(ready_w);
        let _ = match &outcome {
            Ok(()) => ready.write_all(b"ok"),
            Err(error) => ready.write_all(error.to_string().as_bytes()),
        };
        drop(ready);
        if outcome.is_err() {
            // SAFETY: leaving the forked child.
            unsafe { libc::_exit(1) };
        }
        loop {
            // SAFETY: `pause` has no preconditions.
            unsafe { libc::pause() };
        }
    }
    drop(ready_w);
    let mut said = String::new();
    File::from(ready_r).read_to_string(&mut said)?;
    if said != "ok" {
        return Err(io::Error::other(format!(
            "the VM's network namespace could not be made: {said}"
        )));
    }
    u32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))
}

fn holder_child(uid: u32) -> io::Result<()> {
    // SAFETY: plain system calls on this (child) process.
    unsafe {
        check(libc::setgroups(0, std::ptr::null()), "setgroups")?;
        check(libc::setresgid(uid, uid, uid), "setresgid")?;
        check(libc::prctl(libc::PR_SET_KEEPCAPS, 1, 0, 0, 0), "keepcaps")?;
        check(libc::setresuid(uid, uid, uid), "setresuid")?;
        // Changing ids made this process undumpable, which makes its own
        // /proc files root's: it could not write its uid map, nor could
        // `pasta`, the same uid, open its namespaces.
        check(libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0), "dumpable")?;
        // Kept capabilities are not effective after the switch: make them
        // so for `unshare`, which then makes the user namespace this uid's.
        set_capabilities(true)?;
        check(
            libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET),
            "unshare",
        )?;
    }
    std::fs::write("/proc/self/setgroups", "deny").step("setgroups deny")?;
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1")).step("uid map")?;
    std::fs::write("/proc/self/gid_map", format!("{uid} {uid} 1")).step("gid map")?;
    // SAFETY: as above.
    unsafe {
        set_capabilities(false)?;
        // Gone with Firecracker, which this process's parent becomes.
        check(
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0),
            "pdeathsig",
        )?;
    }
    Ok(())
}

/// Every permitted capability effective (`keep`), or none at all.
unsafe fn set_capabilities(keep: bool) -> io::Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: libc::c_int,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const VERSION_3: u32 = 0x2008_0522;
    let mut header = Header {
        version: VERSION_3,
        pid: 0,
    };
    let mut data = [Data::default(); 2];
    // SAFETY: the header and two data structs are what capget/capset take.
    unsafe {
        if keep {
            check(
                i32::try_from(libc::syscall(
                    libc::SYS_capget,
                    &raw mut header,
                    data.as_mut_ptr(),
                ))
                .unwrap_or(-1),
                "capget",
            )?;
            for word in &mut data {
                word.effective = word.permitted;
            }
        }
        check(
            i32::try_from(libc::syscall(
                libc::SYS_capset,
                &raw mut header,
                data.as_ptr(),
            ))
            .unwrap_or(-1),
            "capset",
        )
    }
}

/// `pasta` as the network uid, in the host's network namespace, attached
/// to the holder's. Returns its pid once the namespace is connected.
fn spawn_pasta(
    binary: &Path,
    host: &File,
    net_uid: u32,
    holder: u32,
    pid_file: &Path,
    resolver: Ipv4Addr,
) -> io::Result<u32> {
    let args = network::pasta_args(holder, &pid_file.display().to_string(), resolver);
    let host = host.as_raw_fd();
    let mut command = std::process::Command::new(binary);
    command
        .args(&args)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    // SAFETY: async-signal-safe system calls between fork and exec. From
    // root to the network uid every capability is dropped.
    unsafe {
        command.pre_exec(move || {
            if libc::setns(host, libc::CLONE_NEWNET) != 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(net_uid, net_uid, net_uid) != 0
                || libc::setresuid(net_uid, net_uid, net_uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // `pasta` puts itself in the background once the namespace is up, and
    // the process started here exits.
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "pasta: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let started = Instant::now();
    loop {
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return Ok(pid);
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(io::Error::other("pasta wrote no pid"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both descriptors.
    check(
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) },
        "pipe",
    )?;
    // SAFETY: two new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// An ioctl request code as the C library types it: glibc's `unsigned long`, musl's `int`, which
/// carries the same 32 bits.
#[cfg(not(target_env = "musl"))]
fn ioctl_request(code: u32) -> libc::c_ulong {
    libc::c_ulong::from(code)
}

#[cfg(target_env = "musl")]
fn ioctl_request(code: u32) -> libc::c_int {
    libc::c_int::from_ne_bytes(code.to_ne_bytes())
}

/// A persistent tap the VM's uid may attach to: Firecracker opens it by
/// name.
fn create_tap(name: &str, owner: u32) -> io::Result<()> {
    const TUNSETIFF: u32 = 0x4004_54ca;
    const TUNSETPERSIST: u32 = 0x4004_54cb;
    const TUNSETOWNER: u32 = 0x4004_54cc;
    const IFF_TAP: libc::c_short = 0x0002;
    const IFF_NO_PI: libc::c_short = 0x1000;
    const IFF_VNET_HDR: libc::c_short = 0x4000;
    let tun = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let mut request = ifreq(name);
    request.ifr_ifru.ifru_flags = IFF_TAP | IFF_NO_PI | IFF_VNET_HDR;
    // SAFETY: the ioctls read the request, which lives across the calls.
    unsafe {
        check(
            libc::ioctl(
                tun.as_raw_fd(),
                ioctl_request(TUNSETIFF),
                &raw const request,
            ),
            "creating the VM's tap",
        )?;
        check(
            libc::ioctl(
                tun.as_raw_fd(),
                ioctl_request(TUNSETOWNER),
                libc::c_ulong::from(owner),
            ),
            "giving the VM its tap",
        )?;
        check(
            libc::ioctl(tun.as_raw_fd(), ioctl_request(TUNSETPERSIST), 1_u64),
            "keeping the VM's tap",
        )?;
    }
    Ok(())
}

fn ifreq(name: &str) -> libc::ifreq {
    // SAFETY: an all-zero ifreq is valid.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(name.bytes().take(15)) {
        *slot = libc::c_char::from_ne_bytes([byte]);
    }
    request
}

/// Interface configuration in the current network namespace.
struct Control(OwnedFd);

impl Control {
    fn open() -> io::Result<Self> {
        // SAFETY: a new socket nothing else owns.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        check(fd, "a configuration socket")?;
        // SAFETY: as above.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn add_bridge(&self, name: &str) -> io::Result<()> {
        const SIOCBRADDBR: u32 = 0x89a0;
        let name = CString::new(name)?;
        // SAFETY: the name lives across the call.
        check(
            unsafe {
                libc::ioctl(
                    self.0.as_raw_fd(),
                    ioctl_request(SIOCBRADDBR),
                    name.as_ptr(),
                )
            },
            "creating the VM's bridge",
        )
    }

    fn enslave(&self, bridge: &str, port: &str) -> io::Result<()> {
        const SIOCBRADDIF: u32 = 0x89a2;
        let index = interface_index(port)?;
        let mut request = ifreq(bridge);
        request.ifr_ifru.ifru_ifindex = index;
        // SAFETY: the request lives across the call.
        check(
            unsafe {
                libc::ioctl(
                    self.0.as_raw_fd(),
                    ioctl_request(SIOCBRADDIF),
                    &raw const request,
                )
            },
            "bridging the VM's network",
        )
    }

    fn up(&self, name: &str) -> io::Result<()> {
        let mut request = ifreq(name);
        // SAFETY: the request lives across both calls.
        unsafe {
            check(
                libc::ioctl(self.0.as_raw_fd(), libc::SIOCGIFFLAGS, &raw mut request),
                "reading a link's flags",
            )?;
            request.ifr_ifru.ifru_flags |= libc::c_short::try_from(libc::IFF_UP).unwrap_or(1);
            check(
                libc::ioctl(self.0.as_raw_fd(), libc::SIOCSIFFLAGS, &raw const request),
                "bringing a link up",
            )
        }
    }
}

fn interface_index(name: &str) -> io::Result<libc::c_int> {
    let name = CString::new(name)?;
    // SAFETY: the name lives across the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        return Err(os("finding an interface"));
    }
    libc::c_int::try_from(index).map_err(|_| io::Error::other("interface index out of range"))
}

/// Kill everything in the slot's cgroups.
pub fn stop(config: &JailConfig, slot: u32) -> io::Result<()> {
    for cgroup in [vm_cgroup(config, slot), net_cgroup(config, slot)] {
        let kill = cgroup.join("cgroup.kill");
        if kill.exists() {
            std::fs::write(kill, "1").step("killing a VM's cgroup")?;
        }
    }
    Ok(())
}

/// After the VM is gone: end its network, remove its cgroups and jail, and
/// give its files back to the service.
pub fn release(config: &JailConfig, slot: u32) -> io::Result<()> {
    let owners = [config.service_uid, config.vm_uid(slot)];
    let vmm = passed_dir(VMM_FD, &owners, "VM's state")?;
    let sockets = passed_dir(SOCKETS_FD, &owners, "VM's sockets")?;
    stop(config, slot)?;
    for cgroup in [vm_cgroup(config, slot), net_cgroup(config, slot)] {
        let started = Instant::now();
        while cgroup.exists() {
            let events = std::fs::read_to_string(cgroup.join("cgroup.events")).unwrap_or_default();
            if events.lines().any(|line| line == "populated 0") {
                std::fs::remove_dir(&cgroup).step("removing a VM's cgroup")?;
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other(format!(
                    "{} did not empty",
                    cgroup.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    hand_over(&vmm, config.service_uid, config.service_gid).step("handing the VM's state back")?;
    hand_over(&sockets, config.service_uid, config.service_gid)
        .step("handing the VM's sockets back")?;
    for dir in [config.jail_dir(slot), config.net_dir(slot)] {
        match std::fs::remove_dir_all(&dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

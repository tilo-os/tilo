//! Micro-VM steps: a command that needs real root (rootful podman or buildah, loop devices,
//! mounts, a VM of its own) runs as root in a guest kernel of its own, while the virtual
//! machine itself is an ordinary job: QEMU, unprivileged, in the strict job sandbox, with
//! `/dev/kvm` as its only device. Nothing runs as root on the host.
//!
//! - **Machine:** QEMU's `microvm` on x86-64 and `virt` on arm64, with virtio-mmio devices only.
//!   The guest gets the step's cores and memory, and `/dev/kvm` of its own (nested
//!   virtualisation) when it asks for KVM.
//! - **Guest:** a kernel and a static busybox, which the caller pins (t builds them as the
//!   outputs of a check: its `deploy/vm-guest`). [`prepare`] writes the initramfs per step: busybox, `init.sh` and
//!   the job file that says what to mount and run.
//! - **Disks, virtio-blk only:**
//!   - the step's image as a read-only filesystem image (ext4 or erofs), its root, with
//!     everything written going copy-on-write to the scratch disk;
//!   - more read-only filesystem images ([`Disk`]), mounted read-only or copy-on-write where
//!     the step expects them (its sources, the outputs of steps before it);
//!   - a sparse scratch disk, an empty ext4 made by [`format_scratch`]: the root's changes,
//!     `/tmp`, `/var/tmp` and `/var/lib/containers`;
//!   - a raw output disk: the guest writes a status line and a tar of the files matching the
//!     step's output patterns, which [`Prepared::finish`] unpacks. The host never mounts what
//!     a guest wrote.
//!
//!   virtio-fs is avoided: it is slow and breaks `SQLite`'s write-ahead log. Disk IO is
//!   synchronous (QEMU's thread pool): the sandbox refuses `io_uring`.
//! - **Output:** the command's stdout and stderr go to a virtio serial port (not a terminal, so
//!   tools behave as with a pipe), the guest's console to the same place: both are QEMU's
//!   stdout, the job's.
//! - **Network:** user-mode, through a passt outside the sandbox ([`Passt`]) that QEMU reaches
//!   on a unix socket (`-netdev stream`): outbound connections and DNS, no way to the node's
//!   loopback, nothing forwarded inwards. QEMU's own job has no network at all.
//!
//! Filesystem images are made without privileges: [`make_image`] runs `mkfs.ext4 -d` in a user
//! namespace that maps the caller to root, so the image's files belong to root.

mod cpio;
pub mod tar;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, Read, Seek};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{ExitStatus, JobSpec, Mount, Root, SpawnError};

/// The guest's init script.
const INIT: &str = include_str!("init.sh");

/// Where the output tar starts on the output disk; the status line is before it.
const OUTPUTS_AT: u64 = 1 << 20;

/// Where QEMU's job sees the step's files.
const WORK: &str = "/vm/work";
const KERNEL: &str = "/vm/kernel";
const ROOT: &str = "/vm/root.img";
const NET: &str = "/vm/net.sock";

fn disk_path(n: usize) -> PathBuf {
    PathBuf::from(format!("/vm/disk{n}.img"))
}

/// The guest's addresses on its user-mode network (passt's view of it).
const GUEST_ADDRESS: &str = "10.0.2.15";
const GATEWAY: &str = "10.0.2.2";
const NETMASK: &str = "255.255.255.0";
/// Queries to this address go to the node's first resolver.
const DNS: &str = "10.0.2.3";

/// What QEMU itself needs besides the guest's memory: its code, device state, IO buffers.
pub const QEMU_OVERHEAD: u64 = 160 << 20;

/// The guest's two files.
#[derive(Debug, Clone)]
pub struct Guest {
    /// The kernel: an ELF `vmlinux` (booted through PVH) on x86-64, an `Image` on arm64.
    pub kernel: PathBuf,
    /// A static busybox, the guest init's only tool.
    pub busybox: PathBuf,
}

/// A read-only filesystem image and where parts of it appear in the guest.
#[derive(Debug, Clone)]
pub struct Disk {
    pub image: PathBuf,
    pub mounts: Vec<DiskMount>,
}

/// A directory of a [`Disk`] at `dst` in the guest.
#[derive(Debug, Clone)]
pub struct DiskMount {
    /// Relative to the image's root; empty for all of it.
    pub path: PathBuf,
    pub dst: PathBuf,
    /// Writable, copy-on-write: what the step changes goes to the scratch disk.
    pub writable: bool,
}

/// One micro-VM step.
#[derive(Debug, Clone)]
pub struct VmSpec {
    pub guest: Guest,
    /// The step's image as a filesystem image ([`make_image`]): the guest's root.
    pub root: PathBuf,
    pub disks: Vec<Disk>,
    /// The scratch disk's size, bytes: an upper bound, as it is sparse.
    pub scratch: u64,
    /// The output disk's size, bytes: room for the outputs' tar.
    pub output: u64,
    /// The command and its whole environment; a program without a `/` is looked up in the
    /// environment's `PATH`.
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    pub hostname: String,
    /// Globs over absolute guest paths (`*` also matches `/`) of the files to bring back.
    pub outputs: Vec<String>,
    pub cpus: u32,
    /// The guest's memory, bytes. QEMU's job needs [`QEMU_OVERHEAD`] more.
    pub memory: u64,
    /// Give the guest `/dev/kvm` of its own, for steps that boot VMs.
    pub kvm: bool,
    /// The unix socket of a [`Passt`], for a network; none without.
    pub network: Option<PathBuf>,
    /// The QEMU binary; `qemu-system-<arch>` on the job's `PATH` by default.
    pub qemu: Option<PathBuf>,
}

/// A step ready to start: the job that runs QEMU, and where its result will be.
#[derive(Debug)]
pub struct Prepared {
    /// The strict job running QEMU. Launch it with the caller's cgroup and stdout.
    pub job: JobSpec,
    output: PathBuf,
}

/// Why a step's result can't be read.
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    /// The guest stopped before saying how its command ended: a kernel panic, the guest init
    /// failing (it says why on the console), or QEMU killed.
    #[error(
        "the VM ended without reporting how its command ended (its console output above says why)"
    )]
    NoStatus,
    /// The guest init failed after the command ran (an output that couldn't be archived).
    #[error("the VM's init failed (its console output above says why)")]
    Guest,
    #[error("reading the VM's outputs: {0}")]
    Io(#[from] io::Error),
}

/// Prepare a step in `dir`, an empty directory of the caller's that outlives the job: the
/// initramfs, the scratch and output disks.
///
/// # Errors
/// [`SpawnError::Invalid`] for a spec that can't run, [`SpawnError::Setup`] when a file could
/// not be written.
pub fn prepare(spec: &VmSpec, dir: &Path) -> Result<Prepared, SpawnError> {
    let setup = |what: &str, e: io::Error| SpawnError::Setup(format!("{what}: {e}"));
    if spec.argv.is_empty() {
        return Err(SpawnError::Invalid("a VM step needs a command".into()));
    }
    let devices = virtio_devices(spec);
    if devices > VIRTIO_TRANSPORTS {
        return Err(SpawnError::Invalid(format!(
            "a VM step with {} disks needs {devices} virtio devices; this machine has {VIRTIO_TRANSPORTS}",
            spec.disks.len()
        )));
    }
    for p in [&spec.guest.kernel, &spec.guest.busybox, &spec.root]
        .into_iter()
        .chain(spec.disks.iter().map(|d| &d.image))
    {
        if !p.is_absolute() || !p.exists() {
            return Err(SpawnError::Invalid(format!(
                "{} is not an existing absolute path",
                p.display()
            )));
        }
    }
    if !dir.is_absolute() {
        return Err(SpawnError::Invalid(format!(
            "{} is not absolute",
            dir.display()
        )));
    }
    write_initramfs(spec, &dir.join("initramfs")).map_err(|e| setup("writing the initramfs", e))?;

    let scratch = dir.join("scratch.img");
    format_scratch(&scratch, spec.scratch).map_err(|e| setup("making the scratch disk", e))?;
    let output = dir.join("output.img");
    let out = std::fs::File::create(&output).map_err(|e| setup("making the output disk", e))?;
    out.set_len(spec.output.max(OUTPUTS_AT.saturating_mul(2)))
        .map_err(|e| setup("making the output disk", e))?;

    let argv = qemu_argv(spec);
    let mut job = JobSpec::new(Root::Host { extra: vec![] }, argv);
    job.mount(Mount::read_write(dir, WORK));
    job.mount(Mount::read_only(&spec.guest.kernel, KERNEL));
    job.mount(Mount::read_only(&spec.root, ROOT));
    for (n, disk) in spec.disks.iter().enumerate() {
        job.mount(Mount::read_only(&disk.image, disk_path(n)));
    }
    if let Some(socket) = &spec.network {
        job.mount(Mount::read_write(socket, NET));
    }
    job.devices.push("/dev/kvm".into());
    job.hostname.clone_from(&spec.hostname);
    Ok(Prepared { job, output })
}

impl Prepared {
    /// How the step's command ended, once its job has: what the guest wrote on the output
    /// disk. With `outputs`, the files it archived are unpacked there (paths relative to the
    /// guest's root: `target/release/app` for `/target/release/app`).
    ///
    /// # Errors
    /// [`VmError::NoStatus`] when the guest wrote no status, [`VmError::Guest`] when its init
    /// failed after the command, and IO errors reading the disk.
    pub fn finish(&self, outputs: Option<&Path>) -> Result<ExitStatus, VmError> {
        let mut disk = std::fs::File::open(&self.output)?;
        let mut head = [0_u8; 64];
        disk.read_exact(&mut head)?;
        let line = head.split(|&b| b == b'\n').next().unwrap_or(&[]);
        let status = std::str::from_utf8(line)
            .ok()
            .and_then(|l| l.strip_prefix("T-VM-STATUS "))
            .ok_or(VmError::NoStatus)?;
        if status == "error" {
            return Err(VmError::Guest);
        }
        let code: i32 = status.trim().parse().map_err(|_| VmError::NoStatus)?;
        if let Some(dest) = outputs {
            disk.seek(io::SeekFrom::Start(OUTPUTS_AT))?;
            let mut reader = io::BufReader::with_capacity(1 << 20, disk);
            tar::unpack(&mut reader, dest)?;
        }
        Ok(ExitStatus {
            code: Some(code),
            signal: None,
        })
    }
}

/// The virtio devices the machine can hold: `microvm` has eight virtio-mmio transports, `virt`
/// thirty-two.
const VIRTIO_TRANSPORTS: usize = if cfg!(target_arch = "aarch64") { 32 } else { 8 };

/// The virtio devices a step needs: the serial ports' device, the random number generator on
/// arm64, the root, scratch and output disks, one per disk, and the network.
fn virtio_devices(spec: &VmSpec) -> usize {
    BASE_DEVICES
        .saturating_add(spec.disks.len())
        .saturating_add(usize::from(spec.network.is_some()))
}

/// The serial ports' device, the random number generator on arm64, and the root, scratch and
/// output disks.
const BASE_DEVICES: usize = if cfg!(target_arch = "aarch64") { 5 } else { 4 };

/// The most disks of its own a step with a network can have on this machine.
pub const MAX_DISKS: usize = VIRTIO_TRANSPORTS
    .saturating_sub(BASE_DEVICES)
    .saturating_sub(1);

// Sources, the checks' /target and a mirror's objects: what t gives a step at most.
const _: () = assert!(MAX_DISKS >= 3);

/// QEMU's command line, with the step's files at their paths in QEMU's job.
fn qemu_argv(spec: &VmSpec) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        spec.qemu
            .clone()
            .map_or_else(|| qemu_binary().into(), Into::into),
    ];
    let mut arg = |a: &str| argv.push(a.into());
    for a in machine(spec.kvm) {
        arg(&a);
    }
    // x86's microvm has only eight virtio transports, which a step's disks and network fill;
    // its guest seeds its random numbers from the CPU (RDRAND) instead. arm64's `virt` has
    // room, and no such instruction to rely on.
    if cfg!(target_arch = "aarch64") {
        arg("-device");
        arg("virtio-rng-device");
    }
    let memory_mib = (spec.memory >> 20).max(256);
    for a in [
        "-nodefaults",
        "-no-user-config",
        "-nographic",
        "-no-reboot",
        "-smp",
        &spec.cpus.max(1).to_string(),
        "-m",
        &format!("{memory_mib}M"),
        "-device",
        "virtio-serial-device",
        "-chardev",
        "file,id=console,path=/dev/stdout",
        "-device",
        "virtconsole,chardev=console",
        "-chardev",
        "file,id=out,path=/dev/stdout",
        "-device",
        "virtserialport,chardev=out,name=t.out",
        "-kernel",
    ] {
        arg(a);
    }
    argv.push(KERNEL.into());
    argv.push("-initrd".into());
    argv.push(format!("{WORK}/initramfs").into());
    argv.push("-append".into());
    let mut cmdline = String::from("console=hvc0 quiet loglevel=1 panic=-1 rdinit=/init");
    if cfg!(target_arch = "x86_64") {
        // microvm has neither ACPI (which would describe each device a second time, next to
        // the command line) nor a keyboard controller: a triple fault resets it at once.
        cmdline.push_str(" reboot=t");
    }
    if spec.network.is_some() {
        let _ = write!(
            cmdline,
            " ip={GUEST_ADDRESS}::{GATEWAY}:{NETMASK}::eth0:off"
        );
    }
    argv.push(cmdline.into());
    let mut drive = |file: &Path, serial: &str, writable: bool| {
        let mut d = OsString::from("file=");
        d.push(escape_option(file.as_os_str()));
        d.push(format!(",if=none,id={serial},format=raw,file.locking=off"));
        // Writes bypass the host's page cache (O_DIRECT): the guest caches them already, and
        // dirty host pages of a multi-GB write (an image pull) were charged to the step's
        // cgroup on top of guest RAM, which the kernel then killed QEMU for. Flushes are
        // ignored: nobody needs the writes on a platter before QEMU exits.
        d.push(if writable {
            ",cache.direct=on,cache.no-flush=on"
        } else {
            ",readonly=on"
        });
        argv.extend(["-drive".into(), d, "-device".into()]);
        argv.push(format!("virtio-blk-device,drive={serial},serial={serial}").into());
    };
    // QEMU sees every file at a fixed path of its own.
    drive(Path::new(ROOT), "root", false);
    drive(&Path::new(WORK).join("scratch.img"), "scratch", true);
    drive(&Path::new(WORK).join("output.img"), "output", true);
    for n in 0..spec.disks.len() {
        drive(&disk_path(n), &format!("disk{n}"), false);
    }
    if spec.network.is_some() {
        argv.extend([
            "-netdev".into(),
            format!("stream,id=net,server=off,addr.type=unix,addr.path={NET}").into(),
            "-device".into(),
            "virtio-net-device,netdev=net".into(),
        ]);
    }
    argv
}

/// The initramfs at `path`: busybox, the init script and the job file.
fn write_initramfs(spec: &VmSpec, path: &Path) -> io::Result<()> {
    let busybox = std::fs::read(&spec.guest.busybox)?;
    let mut cpio = cpio::Cpio::new(Vec::with_capacity(busybox.len().saturating_add(1 << 16)));
    cpio.dir("bin")?;
    cpio.file("bin/busybox", 0o755, &busybox)?;
    cpio.file("init", 0o755, INIT.as_bytes())?;
    cpio.dir("t-vm")?;
    cpio.file("t-vm/job", 0o644, job_file(spec).as_bytes())?;
    std::fs::write(path, cpio.finish()?)
}

/// The machine: `microvm` on x86-64, `virt` on arm64, with KVM.
fn machine(kvm: bool) -> Vec<String> {
    let mut args = vec!["-accel".to_string(), "kvm".to_string(), "-cpu".to_string()];
    if cfg!(target_arch = "aarch64") {
        args.push("host".into());
        let mut m = String::from("virt,gic-version=host");
        if kvm {
            m.push_str(",virtualization=on");
        }
        args.extend(["-M".into(), m]);
    } else {
        // Without KVM asked for, the guest sees no virtualisation extensions.
        args.push(if kvm { "host" } else { "host,-vmx,-svm" }.into());
        args.extend([
            "-M".into(),
            "microvm,acpi=off,isa-serial=off,rtc=off,pit=off,pic=off".into(),
        ]);
    }
    args
}

fn qemu_binary() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "qemu-system-aarch64"
    } else {
        "qemu-system-x86_64"
    }
}

/// A path in a QEMU option value, where a comma is written twice.
fn escape_option(p: &OsStr) -> OsString {
    let mut out = Vec::with_capacity(p.len());
    for &b in p.as_bytes() {
        out.push(b);
        if b == b',' {
            out.push(b',');
        }
    }
    OsString::from(std::ffi::OsStr::from_bytes(&out))
}

/// `s` as one word of POSIX shell.
fn quote(s: &[u8]) -> String {
    let mut out = String::from("'");
    out.push_str(&String::from_utf8_lossy(s).replace('\'', r"'\''"));
    out.push('\'');
    out
}

/// The job file the guest init sources (see `init.sh`).
fn job_file(spec: &VmSpec) -> String {
    let mut f = String::new();
    let q = |s: &OsStr| quote(s.as_bytes());
    let _ = writeln!(f, "T_CWD={}", q(spec.cwd.as_os_str()));
    let _ = writeln!(f, "T_HOSTNAME={}", quote(spec.hostname.as_bytes()));
    let _ = writeln!(f, "T_NET={}", u8::from(spec.network.is_some()));
    let _ = writeln!(f, "T_DNS={DNS}");
    let _ = writeln!(f, "T_MEMORY_MIB={}", spec.memory >> 20);
    f.push_str("t_setup() {\n\t:\n");
    for (n, disk) in spec.disks.iter().enumerate() {
        let _ = writeln!(f, "\tt_disk disk{n} {n}");
        for m in &disk.mounts {
            let mode = if m.writable { "cow" } else { "ro" };
            let _ = writeln!(
                f,
                "\tt_mount {n} {} {} {mode}",
                q(m.path.as_os_str()),
                q(m.dst.as_os_str())
            );
        }
    }
    // The program by its path in the image, looked up in the step's own `PATH`: a busybox
    // built to prefer its applets would otherwise run its own `sh` or `bash` for a bare name.
    // busybox's `env` takes its options, then `NAME=value` words, then the command.
    let path = spec
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map_or(super::spec::DEFAULT_PATH.as_bytes(), |(_, v)| v.as_bytes());
    let _ = writeln!(f, "}}\nT_PATH={}", quote(path));
    f.push_str("t_command() {\n\tprogram=$(t_which ");
    f.push_str(&spec.argv.first().map_or_else(String::new, |a| q(a)));
    f.push_str(") || exit 127\n\texec env -i --");
    for (k, v) in &spec.env {
        let mut kv = k.as_bytes().to_vec();
        kv.push(b'=');
        kv.extend_from_slice(v.as_bytes());
        f.push(' ');
        f.push_str(&quote(&kv));
    }
    f.push_str(" \"$program\"");
    for a in spec.argv.iter().skip(1) {
        f.push(' ');
        f.push_str(&q(a));
    }
    f.push_str("\n}\nt_outputs() {\n\t:\n");
    for o in &spec.outputs {
        let _ = writeln!(f, "\tt_output {}", quote(o.as_bytes()));
    }
    f.push_str("}\n");
    f
}

/// Make `path` an empty, sparse ext4 of `size` bytes for a step's scratch disk. Without a
/// journal (nothing on it outlives the step) and with its inode tables left for the guest to
/// initialise lazily, which it never needs to: it takes about 0.1 s for 64 GiB.
///
/// # Errors
/// The file could not be made, or `mkfs.ext4` failed.
pub fn format_scratch(path: &Path, size: u64) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    file.set_len(size.max(256 << 20))?;
    drop(file);
    let out = Command::new("mkfs.ext4")
        .args([
            "-q",
            "-F",
            "-O",
            "^has_journal",
            "-E",
            "lazy_itable_init=1,nodiscard",
            "-m",
            "0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mkfs.ext4 failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Make `image` a read-only ext4 holding `tree`, with every file owned by root, as a guest's
/// root or another of its disks. `mkfs.ext4 -d` runs in a user namespace of its own that maps
/// the caller to root: the files it reads as the caller's (unreadable ones, such as an image's
/// `/etc/shadow`, included), it records as root's. Sized to fit, with room to spare for
/// metadata.
///
/// # Errors
/// [`SpawnError::Invalid`] for relative paths, [`SpawnError::Setup`] when `mkfs.ext4` could not
/// start or failed.
pub fn make_image(tree: &Path, image: &Path) -> Result<(), SpawnError> {
    use std::os::unix::process::CommandExt;
    let setup = |what: &str, e: &dyn std::fmt::Display| SpawnError::Setup(format!("{what}: {e}"));
    if !image.is_absolute() || !tree.is_absolute() {
        return Err(SpawnError::Invalid(format!(
            "{} or {} is not absolute",
            image.display(),
            tree.display()
        )));
    }
    let (bytes, inodes) = measure(tree);
    // Each file takes whole blocks, which `measure` counted; metadata and slack on top.
    let size = bytes
        .saturating_add(bytes / 8)
        .saturating_add(inodes.saturating_mul(256))
        .saturating_add(64 << 20);
    let file = std::fs::File::create(image).map_err(|e| setup("making the image", &e))?;
    file.set_len(size)
        .map_err(|e| setup("sizing the image", &e))?;
    drop(file);
    let inode_count = inodes
        .saturating_add(inodes / 4)
        .saturating_add(4096)
        .to_string();
    let mut cmd = Command::new("mkfs.ext4");
    // 4 KiB blocks and extents whatever the size: mke2fs.conf's types for small file systems
    // pick 1 KiB blocks without them, which caps one file at 16 GiB ("Ext2 file too big" for a
    // multi-GB image archive).
    cmd.args(["-q", "-F", "-T", "default", "-b", "4096"])
        .args([
            "-O",
            "^has_journal,extents,huge_file,large_file,64bit",
            "-E",
        ])
        .arg("root_owner=0:0,lazy_itable_init=1,nodiscard")
        .args(["-m", "0", "-N", &inode_count, "-d"])
        .arg(tree)
        .arg(image)
        .stdin(Stdio::null());
    let uid_map = format!("0 {} 1", rustix::process::geteuid().as_raw()).into_bytes();
    let gid_map = format!("0 {} 1", rustix::process::getegid().as_raw()).into_bytes();
    // SAFETY: between fork and exec the child only makes system calls on memory prepared
    // before the fork.
    unsafe {
        cmd.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                return Err(io::Error::last_os_error());
            }
            write_proc(c"/proc/self/setgroups", b"deny")?;
            write_proc(c"/proc/self/uid_map", &uid_map)?;
            write_proc(c"/proc/self/gid_map", &gid_map)
        });
    }
    let out = cmd.output().map_err(|e| setup("starting mkfs.ext4", &e))?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(image);
        Err(SpawnError::Setup(format!(
            "mkfs.ext4 -d {} failed: {}",
            tree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Write `data` to the proc file `path`, with nothing but system calls (after a fork).
fn write_proc(path: &std::ffi::CStr, data: &[u8]) -> io::Result<()> {
    // SAFETY: a plain open/write/close of a NUL-terminated path and a live buffer.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = libc::write(fd, data.as_ptr().cast(), data.len());
        let err = io::Error::last_os_error();
        libc::close(fd);
        if usize::try_from(n).ok() == Some(data.len()) {
            Ok(())
        } else {
            Err(err)
        }
    }
}

/// The bytes the files under `dir` take in whole 4 KiB blocks, and how many entries it holds.
fn measure(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut inodes) = (0_u64, 0_u64);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            inodes = inodes.saturating_add(1);
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
                bytes = bytes.saturating_add(4096);
            } else {
                bytes = bytes.saturating_add(meta.len().div_ceil(4096).saturating_mul(4096));
            }
        }
    }
    (bytes, inodes)
}

/// A passt serving one step's network on a unix socket, stopped when dropped. It runs outside
/// the job, as the caller, and confines itself (namespaces, seccomp, no capabilities). The guest
/// may connect out and resolve names; nothing reaches it from outside, and no address leads to
/// the node's loopback (`--no-map-gw`).
#[derive(Debug)]
pub struct Passt {
    child: Child,
}

impl Passt {
    /// Start passt on `socket` (a path that does not exist yet) and wait for it to listen.
    ///
    /// # Errors
    /// passt is missing, failed, or did not listen within five seconds.
    pub fn start(socket: &Path) -> io::Result<Self> {
        let child = Command::new("passt")
            .args(["--foreground", "--one-off", "--quiet", "--no-map-gw"])
            .args([
                "--address",
                GUEST_ADDRESS,
                "--netmask",
                NETMASK,
                "--gateway",
                GATEWAY,
            ])
            .args(["--dns-forward", DNS])
            .args(["-t", "none", "-u", "none"])
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        let mut passt = Self { child };
        let t0 = Instant::now();
        while !socket.exists() {
            if let Some(status) = passt.child.try_wait()? {
                return Err(io::Error::other(format!("passt ended at once ({status})")));
            }
            if t0.elapsed() > Duration::from_secs(5) {
                return Err(io::Error::other("passt did not listen within 5 s"));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(passt)
    }
}

impl Drop for Passt {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmSpec {
        VmSpec {
            guest: Guest {
                kernel: "/k".into(),
                busybox: "/b".into(),
            },
            root: "/root.img".into(),
            disks: vec![Disk {
                image: "/in.img".into(),
                mounts: vec![
                    DiskMount {
                        path: "src".into(),
                        dst: "/src".into(),
                        writable: true,
                    },
                    DiskMount {
                        path: "it's".into(),
                        dst: "/a b".into(),
                        writable: false,
                    },
                ],
            }],
            scratch: 1 << 30,
            output: 1 << 24,
            argv: vec!["bash".into(), "-c".into(), "echo 'hi' $X".into()],
            env: vec![
                ("X".into(), "a b".into()),
                ("PATH".into(), "/usr/bin".into()),
            ],
            cwd: "/src".into(),
            hostname: "job".into(),
            outputs: vec!["/target/out/*".into()],
            cpus: 2,
            memory: 1 << 30,
            kvm: false,
            network: None,
            qemu: None,
        }
    }

    #[test]
    fn the_job_file_quotes_every_word() {
        let f = job_file(&spec());
        assert!(f.contains("T_CWD='/src'\n"), "{f}");
        assert!(f.contains("\tt_disk disk0 0\n"), "{f}");
        assert!(f.contains("\tt_mount 0 'src' '/src' cow\n"), "{f}");
        assert!(f.contains("\tt_mount 0 'it'\\''s' '/a b' ro\n"), "{f}");
        assert!(
            f.contains(
                "program=$(t_which 'bash') || exit 127\n\texec env -i -- 'X=a b' 'PATH=/usr/bin' \"$program\" '-c' 'echo '\\''hi'\\'' $X'\n"
            ),
            "{f}"
        );
        assert!(f.contains("\tt_output '/target/out/*'\n"), "{f}");
        assert!(f.contains("T_PATH='/usr/bin'\n"), "{f}");
    }

    #[test]
    fn the_most_devices_a_step_can_ask_for_fit_the_machine() {
        let mut spec = spec();
        spec.network = Some("/net".into());
        spec.disks = (0..MAX_DISKS).map(|_| spec.disks[0].clone()).collect();
        let argv = qemu_argv(&spec);
        let virtio = argv
            .iter()
            .filter_map(|a| a.to_str())
            .filter(|a| a.starts_with("virtio-") && a.contains("-device"))
            .count();
        assert_eq!(virtio, virtio_devices(&spec));
        let kernel = argv.iter().position(|a| a == "-kernel").unwrap();
        assert_eq!(
            argv.get(kernel.saturating_add(1)),
            Some(&OsString::from(KERNEL)),
            "-kernel takes the kernel: {argv:?}"
        );
        assert!(virtio <= VIRTIO_TRANSPORTS, "{virtio} virtio devices");
        spec.disks.push(spec.disks[0].clone());
        assert!(virtio_devices(&spec) > VIRTIO_TRANSPORTS || cfg!(target_arch = "aarch64"));
    }

    #[test]
    fn commas_in_paths_are_doubled_for_qemu() {
        assert_eq!(
            escape_option(OsStr::new("/a,b/c")),
            OsString::from("/a,,b/c")
        );
    }

    #[test]
    fn a_status_line_is_read_back_and_outputs_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output.img");
        let mut disk = vec![0_u8; usize::try_from(OUTPUTS_AT).unwrap() + 4096];
        disk[..14].copy_from_slice(b"T-VM-STATUS 3\n");
        std::fs::write(&output, &disk).unwrap();
        let p = Prepared {
            job: JobSpec::new(Root::Host { extra: vec![] }, ["true"]),
            output: output.clone(),
        };
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        assert_eq!(p.finish(Some(&out)).unwrap().code, Some(3));

        disk[..18].copy_from_slice(b"T-VM-STATUS error\n");
        std::fs::write(&output, &disk).unwrap();
        assert!(matches!(p.finish(None), Err(VmError::Guest)));
        std::fs::write(&output, vec![0_u8; 4096]).unwrap();
        assert!(matches!(p.finish(None), Err(VmError::NoStatus)));
    }
}

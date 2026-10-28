//! Micro-VM steps: a command that needs real root (rootful podman or buildah, loop devices,
//! mounts, a VM of its own) runs as root in a guest kernel of its own, while the virtual
//! machine itself is an ordinary job: QEMU, unprivileged, in the strict job sandbox, with
//! `/dev/kvm` as its only device. Nothing runs as root on the host.
//!
//! - **Machine:** QEMU's `microvm` on x86-64 and `virt` on arm64, with virtio-mmio devices only.
//!   The guest gets the step's cores and memory, and `/dev/kvm` of its own (nested
//!   virtualisation) when it asks for KVM. Guest memory is a shared memfd (`share=on`), as
//!   vhost-user devices need: it is shmem, charged to QEMU's cgroup, and the guest's page cache
//!   fills all of it, so whoever books the step books its whole memory.
//! - **Guest:** a kernel and a static busybox, which the caller pins. [`prepare`] writes the
//!   initramfs per step: busybox, `init.sh` and the job file that says what to mount and run.
//! - **Inputs, over virtio-fs:** every input, the root included, is a directory of a read-only
//!   share ([`VmSpec::share`]) served by a virtiofsd the caller starts ([`virtiofsd_argv`],
//!   through [`crate::job::helper`]). Each is a layer: the root is an overlay of the layers
//!   [`VmRoot::Share`] names, and each [`ShareMount`] one of its own, every write going to the
//!   scratch disk. Nothing is copied or converted into an image per step: layers already on
//!   the host are shared as they are.
//! - **Writable share:** directories the step writes for the host to read as they are
//!   ([`VmSpec::writable`], a build directory), from a second, writable share.
//! - **Disks, virtio-blk only:**
//!   - a sparse scratch disk, an empty ext4 made by [`format_scratch`]: the root's and the
//!     inputs' changes, `/tmp`, `/var/tmp` and `/var/lib/containers`. A block device rather
//!     than the writable share: virtio-fs breaks `SQLite`'s write-ahead log, and podman's
//!     storage can't sit on it;
//!   - a raw output disk: the guest writes a tar of the files matching the step's output
//!     patterns and then, last, a status line, which [`Prepared::finish`] reads. The host never
//!     mounts what a guest wrote. A written status line means every output and every write to
//!     the writable share is complete ([`Prepared::done`]).
//!
//!   Disk IO is synchronous (QEMU's thread pool): the sandbox refuses `io_uring`.
//! - **Output:** the command's stdout and stderr go to a virtio serial port (not a terminal, so
//!   tools behave as with a pipe), the guest's console to the same place: both are QEMU's
//!   stdout, the job's.
//! - **Network:** user-mode, through a passt outside the sandbox ([`passt_argv`]) that QEMU
//!   reaches on a unix socket (`-netdev stream`): outbound connections and DNS, nothing
//!   forwarded inwards. QEMU's own job has no network at all.

mod cpio;
pub mod tar;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, Read, Seek};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{ExitStatus, JobSpec, Mount, Root, SpawnError};

/// The guest's init script.
const INIT: &str = include_str!("init.sh");

/// Where the output tar starts on the output disk; the status line is before it.
const OUTPUTS_AT: u64 = 1 << 20;

/// The status line's start: written last, so its presence says everything else is written.
const STATUS_LINE: &[u8] = b"T-VM-STATUS ";

/// Where QEMU's job sees the step's files.
const WORK: &str = "/vm/work";
const KERNEL: &str = "/vm/kernel";
const NET: &str = "/vm/net.sock";

/// Where QEMU's job sees the socket of the `n`th share's virtiofsd.
fn share_socket(n: usize) -> PathBuf {
    PathBuf::from(format!("/vm/fs{n}.sock"))
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

/// The guest's root.
#[derive(Debug, Clone)]
pub enum VmRoot {
    /// Layers of the read-only share, by their names under it, top first, stacked with the
    /// root's changes on the scratch disk.
    Share { layers: Vec<String> },
}

/// A virtio-fs share: the socket of the virtiofsd serving it, and the tag the guest mounts it by.
#[derive(Debug, Clone)]
pub struct Share {
    /// The host path of the socket; the virtiofsd listens on it when the step starts.
    pub socket: PathBuf,
    pub tag: String,
}

/// Layers of the read-only share stacked at `dst` in the guest, copy-on-write: what the step
/// changes goes to the scratch disk.
#[derive(Debug, Clone)]
pub struct ShareMount {
    /// Names under the share, top first.
    pub layers: Vec<String>,
    pub dst: PathBuf,
}

/// One micro-VM step.
#[derive(Debug, Clone)]
pub struct VmSpec {
    pub guest: Guest,
    pub root: VmRoot,
    /// The read-only share holding every layer the step reads.
    pub share: Share,
    /// Inputs besides the root: its sources, the outputs of steps before it.
    pub inputs: Vec<ShareMount>,
    /// The writable share, when the step has directories to write for the host.
    pub writable_share: Option<Share>,
    /// (name under the writable share, guest path): directories the host reads as the step
    /// left them.
    pub writable: Vec<(String, PathBuf)>,
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
    /// The unix socket of a passt ([`passt_argv`]), for a network; none without.
    pub network: Option<PathBuf>,
    /// The QEMU binary; `qemu-system-<arch>` on the job's `PATH` by default.
    pub qemu: Option<PathBuf>,
}

impl VmSpec {
    /// The shares, in the order QEMU gets them.
    fn shares(&self) -> impl Iterator<Item = &Share> {
        std::iter::once(&self.share).chain(self.writable_share.as_ref())
    }
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
    /// The guest init failed (a share or input it couldn't mount, an output that couldn't be
    /// archived).
    #[error("the VM's init failed (its console output above says why)")]
    Guest,
    #[error("reading the VM's outputs: {0}")]
    Io(#[from] io::Error),
}

/// A name under a share as the guest's overlay options and shell take it.
fn check_name(name: &str) -> Result<(), SpawnError> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', ':', ',', '\\', '\n', '\0']);
    if bad {
        return Err(SpawnError::Invalid(format!(
            "{name:?} can't name a directory of a share: it must be one path component without `:`, `,` or `\\`"
        )));
    }
    Ok(())
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
    let VmRoot::Share { layers } = &spec.root;
    if layers.is_empty() {
        return Err(SpawnError::Invalid("a VM step's root needs a layer".into()));
    }
    let names = layers
        .iter()
        .chain(spec.inputs.iter().flat_map(|i| &i.layers))
        .chain(spec.writable.iter().map(|(n, _)| n));
    for name in names {
        check_name(name)?;
    }
    if spec.inputs.iter().any(|i| i.layers.is_empty()) {
        return Err(SpawnError::Invalid(
            "an input of a VM step needs a layer".into(),
        ));
    }
    if !spec.writable.is_empty() && spec.writable_share.is_none() {
        return Err(SpawnError::Invalid(
            "a VM step with writable directories needs a writable share".into(),
        ));
    }
    let devices = virtio_devices(spec);
    if devices > VIRTIO_TRANSPORTS {
        return Err(SpawnError::Invalid(format!(
            "a VM step needs {devices} virtio devices; this machine has {VIRTIO_TRANSPORTS}"
        )));
    }
    for p in [&spec.guest.kernel, &spec.guest.busybox] {
        if !p.is_absolute() || !p.exists() {
            return Err(SpawnError::Invalid(format!(
                "{} is not an existing absolute path",
                p.display()
            )));
        }
    }
    for p in spec.shares().map(|s| &s.socket).chain([&dir.to_path_buf()]) {
        if !p.is_absolute() {
            return Err(SpawnError::Invalid(format!(
                "{} is not absolute",
                p.display()
            )));
        }
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
    for (n, share) in spec.shares().enumerate() {
        job.mount(Mount::read_write(&share.socket, share_socket(n)));
    }
    if let Some(socket) = &spec.network {
        job.mount(Mount::read_write(socket, NET));
    }
    job.devices.push("/dev/kvm".into());
    job.hostname.clone_from(&spec.hostname);
    Ok(Prepared { job, output })
}

impl Prepared {
    /// Whether the guest has written its status line: everything it writes comes before it, so
    /// the step's result is complete even while QEMU still runs (a guest that never powers
    /// off).
    pub fn done(&self) -> bool {
        let mut head = [0_u8; 64];
        let read = std::fs::File::open(&self.output).and_then(|mut d| d.read_exact(&mut head));
        read.is_ok() && head.starts_with(STATUS_LINE) && head.contains(&b'\n')
    }

    /// How the step's command ended, once its guest wrote its status: what it wrote on the
    /// output disk. With `outputs`, the files it archived are unpacked there (paths relative
    /// to the guest's root: `target/release/app` for `/target/release/app`).
    ///
    /// # Errors
    /// [`VmError::NoStatus`] when the guest wrote no status, [`VmError::Guest`] when its init
    /// failed, and IO errors reading the disk.
    pub fn finish(&self, outputs: Option<&Path>) -> Result<ExitStatus, VmError> {
        let mut disk = std::fs::File::open(&self.output)?;
        let mut head = [0_u8; 64];
        disk.read_exact(&mut head)?;
        let line = head.split(|&b| b == b'\n').next().unwrap_or(&[]);
        let status = line
            .strip_prefix(STATUS_LINE)
            .and_then(|l| std::str::from_utf8(l).ok())
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

/// The serial ports' device, the random number generator on arm64, and the scratch and output
/// disks.
const BASE_DEVICES: usize = if cfg!(target_arch = "aarch64") { 4 } else { 3 };

// Both shares and a network fit any machine.
const _: () = assert!(BASE_DEVICES.saturating_add(3) <= VIRTIO_TRANSPORTS);

/// The virtio devices a step needs: the base ones, one per share, and the network.
fn virtio_devices(spec: &VmSpec) -> usize {
    BASE_DEVICES
        .saturating_add(spec.shares().count())
        .saturating_add(usize::from(spec.network.is_some()))
}

/// QEMU's command line, with the step's files at their paths in QEMU's job.
fn qemu_argv(spec: &VmSpec) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        spec.qemu
            .clone()
            .map_or_else(|| qemu_binary().into(), Into::into),
    ];
    let memory_mib = (spec.memory >> 20).max(256);
    let mut arg = |a: &str| argv.push(a.into());
    for a in machine(spec.kvm) {
        arg(&a);
    }
    // x86's microvm has only eight virtio transports; its guest seeds its random numbers from
    // the CPU (RDRAND) instead. arm64's `virt` has room, and no such instruction to rely on.
    if cfg!(target_arch = "aarch64") {
        arg("-device");
        arg("virtio-rng-device");
    }
    // vhost-user devices (virtiofsd) map the guest's memory: it must be a shared file.
    for a in [
        "-object",
        &format!("memory-backend-memfd,id=mem,size={memory_mib}M,share=on"),
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
    let mut drive = |file: &Path, serial: &str| {
        let mut d = OsString::from("file=");
        d.push(escape_option(file.as_os_str()));
        d.push(format!(",if=none,id={serial},format=raw,file.locking=off"));
        // Writes bypass the host's page cache (O_DIRECT): the guest caches them already, and
        // dirty host pages of a multi-GB write (an image pull) were charged to the step's
        // cgroup on top of guest RAM, which the kernel then killed QEMU for. Flushes are
        // ignored: nobody needs the writes on a platter before QEMU exits.
        d.push(",cache.direct=on,cache.no-flush=on");
        argv.extend(["-drive".into(), d, "-device".into()]);
        argv.push(format!("virtio-blk-device,drive={serial},serial={serial}").into());
    };
    // QEMU sees every file at a fixed path of its own.
    drive(&Path::new(WORK).join("scratch.img"), "scratch");
    drive(&Path::new(WORK).join("output.img"), "output");
    for (n, share) in spec.shares().enumerate() {
        let mut socket = OsString::from(format!("socket,id=fs{n},path="));
        socket.push(escape_option(share_socket(n).as_os_str()));
        argv.extend(["-chardev".into(), socket, "-device".into()]);
        argv.push(
            format!(
                "vhost-user-fs-device,chardev=fs{n},tag={}",
                share.tag.replace(',', ",,")
            )
            .into(),
        );
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
        let mut m = String::from("virt,gic-version=host,memory-backend=mem");
        if kvm {
            m.push_str(",virtualization=on");
        }
        args.extend(["-M".into(), m]);
    } else {
        // Without KVM asked for, the guest sees no virtualisation extensions.
        args.push(if kvm { "host" } else { "host,-vmx,-svm" }.into());
        args.extend([
            "-M".into(),
            "microvm,acpi=off,isa-serial=off,rtc=off,pit=off,pic=off,memory-backend=mem".into(),
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
    let qs = |s: &str| quote(s.as_bytes());
    let _ = writeln!(f, "T_CWD={}", q(spec.cwd.as_os_str()));
    let _ = writeln!(f, "T_HOSTNAME={}", qs(&spec.hostname));
    let _ = writeln!(f, "T_NET={}", u8::from(spec.network.is_some()));
    let _ = writeln!(f, "T_DNS={DNS}");
    let _ = writeln!(f, "T_MEMORY_MIB={}", spec.memory >> 20);
    let _ = writeln!(f, "T_SHARE_TAG={}", qs(&spec.share.tag));
    let rw = spec.writable_share.as_ref().map_or("", |s| s.tag.as_str());
    let _ = writeln!(f, "T_RW_TAG={}", qs(rw));
    // The root's layers, top first, for stage 1.
    f.push_str("t_layers() {\n\t:\n");
    let VmRoot::Share { layers } = &spec.root;
    for l in layers {
        let _ = writeln!(f, "\tt_layer {}", qs(l));
    }
    f.push_str("}\nt_setup() {\n\t:\n");
    for input in &spec.inputs {
        let _ = write!(f, "\tt_input {}", q(input.dst.as_os_str()));
        for l in &input.layers {
            let _ = write!(f, " {}", qs(l));
        }
        f.push('\n');
    }
    for (name, dst) in &spec.writable {
        let _ = writeln!(f, "\tt_writable {} {}", qs(name), q(dst.as_os_str()));
    }
    // The program by its path in the root, looked up in the step's own `PATH`: a busybox
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
        let _ = writeln!(f, "\tt_output {}", qs(o));
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

/// virtiofsd's command line for a share of `shared_dir` on `socket`, as the caller starts it
/// through [`crate::job::helper::spawn`] (with `nofile_max`: it holds a descriptor per inode
/// the guest looked up). `--sandbox none`: the helper's own mount namespace already holds only
/// what it shares. `--announce-submounts`: each layer is a mount of its own, and the guest must
/// see each as a filesystem of its own (inode numbers repeat across them). `--xattr`: the
/// layers' opaque directories are user extended attributes the guest's overlay reads.
pub fn virtiofsd_argv(shared_dir: &Path, socket: &Path, readonly: bool) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["virtiofsd".into(), "--socket-path".into()];
    argv.push(socket.into());
    argv.push("--shared-dir".into());
    argv.push(shared_dir.into());
    for a in [
        "--sandbox",
        "none",
        "--cache",
        "always",
        "--announce-submounts",
        "--xattr",
    ] {
        argv.push(a.into());
    }
    if readonly {
        argv.push("--readonly".into());
    }
    argv
}

/// passt's command line serving one step's network on `socket` (a path that does not exist
/// yet), as the caller starts it through [`crate::job::helper::spawn`] in a network namespace
/// of the step's own. The guest may connect out and resolve names; nothing reaches it from
/// outside. With `gateway_to_loopback` the guest's gateway address leads to that namespace's
/// loopback, where only what the caller put there for the step listens (a relay to its cache);
/// without, no address leads to any loopback.
pub fn passt_argv(socket: &Path, gateway_to_loopback: bool) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["passt", "--foreground", "--one-off", "--quiet"]
        .map(OsString::from)
        .to_vec();
    if !gateway_to_loopback {
        argv.push("--no-map-gw".into());
    }
    // Frames of up to 65520 bytes, passt's largest and its default, said here as the guest
    // sets the same MTU itself (init.sh: it takes no DHCP that would tell it).
    for a in [
        "--mtu",
        "65520",
        "--address",
        GUEST_ADDRESS,
        "--netmask",
        NETMASK,
        "--gateway",
        GATEWAY,
        "--dns-forward",
        DNS,
        "-t",
        "none",
        "-u",
        "none",
        "--socket",
    ] {
        argv.push(a.into());
    }
    argv.push(socket.into());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guest's root copies every change up in full, so what a step changed is all in its
    /// upper directory: a moved directory or a chmod under `redirect_dir` or `metacopy` would live in
    /// overlay xattrs, and a layer made of the upper directory would lose it.
    #[test]
    fn the_guest_root_overlay_copies_changes_up_in_full() {
        let mount = INIT
            .lines()
            .skip_while(|l| !l.contains("mount -t overlay"))
            .take(3)
            .collect::<String>();
        assert!(
            mount.contains("redirect_dir=off") && mount.contains("metacopy=off"),
            "{mount}"
        );
    }

    fn spec() -> VmSpec {
        VmSpec {
            guest: Guest {
                kernel: "/k".into(),
                busybox: "/b".into(),
            },
            root: VmRoot::Share {
                layers: vec!["i-top".into(), "i-base".into()],
            },
            share: Share {
                socket: "/w/ro.sock".into(),
                tag: "inputs".into(),
            },
            inputs: vec![
                ShareMount {
                    layers: vec!["s-1".into()],
                    dst: "/src".into(),
                },
                ShareMount {
                    layers: vec!["it's".into()],
                    dst: "/a b".into(),
                },
            ],
            writable_share: Some(Share {
                socket: "/w/rw.sock".into(),
                tag: "writes".into(),
            }),
            writable: vec![("target".into(), "/target".into())],
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
        assert!(
            f.contains("T_SHARE_TAG='inputs'\nT_RW_TAG='writes'\n"),
            "{f}"
        );
        assert!(f.contains("\tt_layer 'i-top'\n\tt_layer 'i-base'\n"), "{f}");
        assert!(f.contains("\tt_input '/src' 's-1'\n"), "{f}");
        assert!(f.contains("\tt_input '/a b' 'it'\\''s'\n"), "{f}");
        assert!(f.contains("\tt_writable 'target' '/target'\n"), "{f}");
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
    fn every_share_is_a_vhost_user_fs_device_on_shared_memory() {
        let mut spec = spec();
        spec.network = Some("/net".into());
        let argv = qemu_argv(&spec);
        let words: Vec<&str> = argv.iter().filter_map(|a| a.to_str()).collect();
        assert!(
            words.contains(&"memory-backend-memfd,id=mem,size=1024M,share=on"),
            "{words:?}"
        );
        let machine = words.iter().position(|a| *a == "-M").unwrap();
        assert!(
            words[machine + 1].contains("memory-backend=mem"),
            "{words:?}"
        );
        assert!(
            words.contains(&"socket,id=fs0,path=/vm/fs0.sock"),
            "{words:?}"
        );
        assert!(
            words.contains(&"vhost-user-fs-device,chardev=fs0,tag=inputs"),
            "{words:?}"
        );
        assert!(
            words.contains(&"vhost-user-fs-device,chardev=fs1,tag=writes"),
            "{words:?}"
        );
        assert!(!words.iter().any(|w| w.contains("root.img")), "{words:?}");
        // The serial ports ride on the one serial device; every other device is a transport.
        let virtio = words
            .iter()
            .filter(|a| a.starts_with("virtio-") || a.starts_with("vhost-user-fs-device"))
            .count();
        assert_eq!(virtio, virtio_devices(&spec), "{words:?}");
        assert!(virtio <= VIRTIO_TRANSPORTS);
        let kernel = words.iter().position(|a| *a == "-kernel").unwrap();
        assert_eq!(
            words.get(kernel + 1),
            Some(&KERNEL),
            "-kernel takes the kernel"
        );
    }

    #[test]
    fn a_share_name_that_would_break_the_guests_mounts_is_refused() {
        for bad in ["", "a/b", "a:b", "a,b", ".."] {
            let mut s = spec();
            s.root = VmRoot::Share {
                layers: vec![bad.into()],
            };
            let dir = tempfile::tempdir().unwrap();
            assert!(
                matches!(prepare(&s, dir.path()), Err(SpawnError::Invalid(_))),
                "{bad:?} accepted"
            );
        }
    }

    #[test]
    fn virtiofsd_shares_read_only_with_submounts_and_xattrs() {
        let argv = virtiofsd_argv(Path::new("/share"), Path::new("/sock/fs.sock"), true);
        let words: Vec<&str> = argv.iter().filter_map(|a| a.to_str()).collect();
        assert_eq!(
            words,
            [
                "virtiofsd",
                "--socket-path",
                "/sock/fs.sock",
                "--shared-dir",
                "/share",
                "--sandbox",
                "none",
                "--cache",
                "always",
                "--announce-submounts",
                "--xattr",
                "--readonly"
            ]
        );
        assert!(
            !virtiofsd_argv(Path::new("/s"), Path::new("/x"), false)
                .iter()
                .any(|a| a == "--readonly")
        );
    }

    #[test]
    fn commas_in_paths_are_doubled_for_qemu() {
        assert_eq!(
            escape_option(OsStr::new("/a,b/c")),
            OsString::from("/a,,b/c")
        );
    }

    #[test]
    fn the_init_flushes_only_the_output_disk_before_it_stops_the_machine() {
        // A global sync reaches the host's whole filesystem through every virtio-fs share,
        // and a busy node took longer to sync it than t lets a step make no progress.
        let commands: Vec<&str> = INIT
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .collect();
        assert!(
            !commands
                .iter()
                .any(|l| l.split([' ', ';', '&', '|']).any(|w| w == "sync")),
            "the init syncs every filesystem"
        );
        let finish = INIT
            .split("finish() {")
            .nth(1)
            .and_then(|f| f.split("\n}").next())
            .expect("the init has finish()");
        let flush = finish
            .find("conv=notrunc,fsync")
            .expect("finish flushes the output disk");
        assert!(
            flush < finish.find("reboot -f").expect("finish stops the machine"),
            "{finish}"
        );
    }

    #[test]
    fn a_status_line_is_read_back_and_outputs_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output.img");
        let mut disk = vec![0_u8; usize::try_from(OUTPUTS_AT).unwrap() + 4096];
        std::fs::write(&output, &disk).unwrap();
        let p = Prepared {
            job: JobSpec::new(Root::Host { extra: vec![] }, ["true"]),
            output: output.clone(),
        };
        assert!(!p.done());
        disk[..14].copy_from_slice(b"T-VM-STATUS 3\n");
        std::fs::write(&output, &disk).unwrap();
        assert!(p.done());
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        assert_eq!(p.finish(Some(&out)).unwrap().code, Some(3));

        disk[..18].copy_from_slice(b"T-VM-STATUS error\n");
        std::fs::write(&output, &disk).unwrap();
        assert!(matches!(p.finish(None), Err(VmError::Guest)));
        std::fs::write(&output, vec![0_u8; 4096]).unwrap();
        assert!(matches!(p.finish(None), Err(VmError::NoStatus)));
        assert!(!p.done());
    }
}

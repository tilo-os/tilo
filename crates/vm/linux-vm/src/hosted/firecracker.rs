//! Firecracker, the hypervisor of hosted VMs: its configuration, its API and the
//! snapshot files. Every VM runs with dirty-page tracking, so a suspend writes a
//! diff snapshot merged into a sparse memory file the size of the guest's RAM. A
//! restore maps that file privately and needs it unchanged while the VM runs, so
//! the state file is removed while it runs and only a completed suspend writes it
//! back; a clone of a template links the template's memory file and copies it on
//! its first suspend.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use thiserror::Error;

use tilo_server_protocol::hosted_vm::VmSize;

/// The guest's context id: hybrid vsock gives every VM its own namespace.
pub const GUEST_CID: u32 = 3;

#[derive(Debug, Error)]
pub enum FirecrackerError {
    #[error("firecracker's API socket did not appear")]
    NoApi,
    #[error("firecracker API {path}: {status} {message}")]
    Api {
        path: String,
        status: u16,
        message: String,
    },
    #[error("firecracker API: {0}")]
    Io(#[from] io::Error),
}

/// How hosted VMs boot: the same for every VM of a host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    pub firecracker: PathBuf,
    /// An uncompressed `vmlinux` with virtio-mmio, vsock and FUSE built in.
    pub kernel: PathBuf,
    pub initramfs: Option<PathBuf>,
    pub cmdline: String,
    /// A raw ext4 image every VM's disk starts as a copy of (reflinked where the filesystem can),
    /// then grown to the VM's size. Without one, a fresh disk is a blank ext4 filesystem.
    pub base_disk: Option<PathBuf>,
}

impl Machine {
    pub const DEFAULT_CMDLINE: &'static str = "console=ttyS0 reboot=k panic=1 quiet";
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmFiles {
    /// Durable: the meter and console log, and [`Self::vmm`].
    pub state: PathBuf,
    /// Sockets, short-lived: the API and vsock sockets. Kept short, since a
    /// Unix socket path has at most 107 bytes.
    pub run: PathBuf,
    /// Firecracker runs in a jail ([`super::jail`]) and sees [`Self::vmm`]
    /// as `/vm`, [`Self::run`] as `/s` and the guest kernel's directory as
    /// `/guest`.
    pub jailed: bool,
}

impl VmFiles {
    /// What Firecracker itself reads and writes: the disk, the snapshot,
    /// its configuration. The VM's own while it runs jailed; the meter is
    /// never in here.
    #[must_use]
    pub fn vmm(&self) -> PathBuf {
        self.state.join("vmm")
    }

    #[must_use]
    pub fn disk(&self) -> PathBuf {
        self.vmm().join("disk.raw")
    }

    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.vmm().join("firecracker.json")
    }

    #[must_use]
    pub fn console_log(&self) -> PathBuf {
        self.state.join("console.log")
    }

    #[must_use]
    pub fn meter(&self) -> PathBuf {
        self.state.join("meter.json")
    }

    #[must_use]
    pub fn snapshot(&self) -> SnapshotFiles {
        let vmm = self.vmm();
        SnapshotFiles {
            state: vmm.join("vm.state"),
            memory: vmm.join("vm.mem"),
            new_state: vmm.join("vm.state.new"),
            diff: vmm.join("vm.diff"),
        }
    }

    /// Present while the VM is a branch whose guest has not yet forgotten the sessions of the VM it
    /// was made from ([`branch_disk`]): beside the meter, never the VM's own to remove.
    #[must_use]
    pub fn branched(&self) -> PathBuf {
        self.state.join("branched")
    }

    #[must_use]
    pub fn api_socket(&self) -> PathBuf {
        self.run.join("api.sock")
    }

    #[must_use]
    pub fn vsock_socket(&self) -> PathBuf {
        self.run.join("v.sock")
    }

    /// `path` as Firecracker names it: the same path unjailed; in a jail,
    /// under `/vm`, `/s` or (for anything else, the guest kernel's files)
    /// `/guest`.
    #[must_use]
    pub fn for_vmm(&self, path: &Path) -> String {
        if !self.jailed {
            return path.display().to_string();
        }
        let vmm = self.vmm();
        let inside = |dir: &str, rest: &Path| Path::new(dir).join(rest).display().to_string();
        if let Ok(rest) = path.strip_prefix(&vmm) {
            inside(super::jail::VMM_DIR, rest)
        } else if let Ok(rest) = path.strip_prefix(&self.run) {
            inside(super::jail::SOCKETS_DIR, rest)
        } else {
            inside(
                super::jail::GUEST_DIR,
                Path::new(path.file_name().unwrap_or_default()),
            )
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotFiles {
    /// Device and vCPU state. Present only while the snapshot is valid.
    pub state: PathBuf,
    /// The guest's whole memory, sparse.
    pub memory: PathBuf,
    /// A suspend's state, until its diff is merged.
    pub new_state: PathBuf,
    /// A suspend's dirty pages, until merged into `memory`.
    pub diff: PathBuf,
}

impl SnapshotFiles {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.state.is_file() && self.memory.is_file()
    }

    /// The VM was restored and now runs on: the snapshot no longer matches
    /// its disk. The memory file stays as the base the next diff merges
    /// into.
    pub fn in_use(&self) -> io::Result<()> {
        remove_if_present(&self.state)
    }

    pub fn discard(&self) -> io::Result<()> {
        for path in [&self.state, &self.memory, &self.new_state, &self.diff] {
            remove_if_present(path)?;
        }
        Ok(())
    }

    /// Where the next suspend writes its memory: the base itself after a
    /// cold boot, else a diff to merge.
    #[must_use]
    pub fn next_memory_target(&self) -> &Path {
        if self.memory.is_file() {
            &self.diff
        } else {
            &self.memory
        }
    }

    /// Finish a suspend whose files Firecracker wrote and which has exited:
    /// merge the diff, then make the state valid. Memory still shared with a
    /// template is copied first: the diff goes into the clone's own copy.
    pub fn complete(&self) -> io::Result<()> {
        if self.diff.is_file() {
            if shared(&self.memory) {
                let private = self.memory.with_extension("mem.private");
                copy_sparse(&self.memory, &private)?;
                std::fs::rename(&private, &self.memory)?;
            }
            merge_diff(&self.diff, &self.memory)?;
            std::fs::remove_file(&self.diff)?;
        }
        std::fs::rename(&self.new_state, &self.state)
    }

    #[must_use]
    pub fn bytes_on_disk(&self) -> u64 {
        allocated(&self.state).saturating_add(allocated(&self.memory))
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Whether `path` has other names: a clone's memory still shared with its
/// template.
#[must_use]
pub fn shared(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.nlink() > 1)
}

/// Copy `from` to `to`, sharing its blocks where the filesystem can and
/// keeping its holes, as the service's own to write.
fn copy_sparse(from: &Path, to: &Path) -> io::Result<()> {
    let _ = std::fs::remove_file(to);
    let status = std::process::Command::new("cp")
        .arg("--reflink=auto")
        .arg("--sparse=always")
        .arg(from)
        .arg(to)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "could not copy {}",
            from.display()
        )));
    }
    // The copy takes the source's mode, and a guest image or a template is
    // read-only.
    std::fs::set_permissions(
        to,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
}

/// Make `files`' VM a clone of `template`'s snapshot: its memory linked to
/// the template's, its state copied, and its disk a reflinked copy of the
/// template's, written last, as a VM is what its disk says.
pub fn clone_vm(template: &VmFiles, files: &VmFiles) -> io::Result<()> {
    let (from, to) = (template.snapshot(), files.snapshot());
    std::fs::create_dir_all(files.vmm())?;
    to.discard()?;
    std::fs::hard_link(&from.memory, &to.memory)?;
    copy_sparse(&from.state, &to.state)?;
    let partial = files.disk().with_extension("raw.partial");
    copy_sparse(&template.disk(), &partial)?;
    std::fs::rename(&partial, files.disk())
}

/// Make `to`'s VM a branch of `from`'s: a copy of its disk alone, sharing its blocks where the
/// filesystem can, grown to `size`. No memory, no vCPU state and no configuration go with it, so it
/// boots cold. The disk is written under another name and renamed last, as a VM is what its disk
/// says: a copy cut short leaves no disk.
pub fn branch_disk(from: &VmFiles, to: &VmFiles, size: VmSize) -> io::Result<()> {
    std::fs::create_dir_all(to.vmm())?;
    let partial = to.disk().with_extension("raw.partial");
    let made = copy_sparse(&from.disk(), &partial).and_then(|()| grow_disk(&partial, size));
    if let Err(error) = made {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, to.disk())
}

#[must_use]
pub fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).map_or(0, |meta| meta.blocks().saturating_mul(512))
}

/// Firecracker's whole configuration for a cold boot; with a network
/// limited to `network_mbit` each way when given ([`super::network`]).
#[must_use]
pub fn config_json(
    machine: &Machine,
    size: VmSize,
    files: &VmFiles,
    network_mbit: Option<u32>,
) -> String {
    let path = |path: &Path| files.for_vmm(path);
    let mut cmdline = machine.cmdline.clone();
    if network_mbit.is_some() {
        cmdline.push(' ');
        cmdline.push_str(&super::network::kernel_ip_arg());
    }
    let mut boot = json!({
        "kernel_image_path": path(&machine.kernel),
        "boot_args": cmdline,
    });
    if let (Some(initramfs), Some(boot)) = (&machine.initramfs, boot.as_object_mut()) {
        boot.insert("initrd_path".into(), json!(path(initramfs)));
    }
    let mut config = json!({
        "boot-source": boot,
        "drives": [{
            "drive_id": "disk",
            "path_on_host": path(&files.disk()),
            "is_root_device": machine.initramfs.is_none(),
            "is_read_only": false,
        }],
        "machine-config": {
            "vcpu_count": size.vcpus(),
            "mem_size_mib": size.memory_mib(),
            "track_dirty_pages": true,
        },
        "vsock": {
            "guest_cid": GUEST_CID,
            "uds_path": path(&files.vsock_socket()),
        },
        // The balloon starts holding nothing and holds back what the host
        // asks for under pressure ([`Api::balloon`]). `deflate_on_oom` is
        // what keeps a guest the balloon pushed too far alive: it gives the
        // memory back itself rather than the workspace losing a page, or the
        // kernel killing what holds it.
        "balloon": {
            "amount_mib": 0,
            "deflate_on_oom": true,
            "stats_polling_interval_s": 0,
        },
        // Randomness for the guest's TLS from its first moment.
        "entropy": {},
    });
    if let (Some(mbit), Some(config)) = (network_mbit, config.as_object_mut()) {
        config.insert(
            "network-interfaces".into(),
            json!([super::network::interface_json(mbit)]),
        );
    }
    config.to_string()
}

#[must_use]
pub fn boot_argv(machine: &Machine, files: &VmFiles) -> Vec<String> {
    vec![
        machine.firecracker.display().to_string(),
        "--api-sock".to_string(),
        files.api_socket().display().to_string(),
        "--config-file".to_string(),
        files.config().display().to_string(),
    ]
}

/// `firecracker` waiting to be told which snapshot to load.
#[must_use]
pub fn restore_argv(machine: &Machine, files: &VmFiles) -> Vec<String> {
    vec![
        machine.firecracker.display().to_string(),
        "--api-sock".to_string(),
        files.api_socket().display().to_string(),
    ]
}

#[derive(Clone, Debug)]
pub struct Api {
    socket: PathBuf,
}

impl Api {
    #[must_use]
    pub const fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Wait for the socket of a just-started Firecracker.
    pub fn wait(&self, patience: Duration) -> Result<(), FirecrackerError> {
        let started = Instant::now();
        while started.elapsed() < patience {
            if tilo_files::native::socket::connect(&self.socket).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(FirecrackerError::NoApi)
    }

    pub fn pause(&self) -> Result<(), FirecrackerError> {
        self.call("PATCH", "/vm", &json!({ "state": "Paused" }))
    }

    pub fn resume(&self) -> Result<(), FirecrackerError> {
        self.call("PATCH", "/vm", &json!({ "state": "Resumed" }))
    }

    /// A diff snapshot: the vCPU and device state to `state`, the pages
    /// dirtied since boot or restore to `memory` (paths as Firecracker
    /// sees them, [`VmFiles::for_vmm`]).
    pub fn snapshot(&self, state: &str, memory: &str) -> Result<(), FirecrackerError> {
        self.call(
            "PUT",
            "/snapshot/create",
            &json!({
                "snapshot_type": "Diff",
                "snapshot_path": state,
                "mem_file_path": memory,
            }),
        )
    }

    /// Load a snapshot, its memory mapped from `memory` on demand, and run
    /// (paths as Firecracker sees them).
    pub fn restore(&self, state: &str, memory: &str) -> Result<(), FirecrackerError> {
        self.call(
            "PUT",
            "/snapshot/load",
            &json!({
                "snapshot_path": state,
                "mem_backend": {
                    "backend_type": "File",
                    "backend_path": memory,
                },
                "track_dirty_pages": true,
                "resume_vm": true,
            }),
        )
    }

    /// Move the VM's balloon to `amount_mib`: the guest's usable memory shrinks by that much, which
    /// the host gets back. A VM whose Firecracker has no balloon device answers with a client
    /// error, which [`HostedVm::balloon`](super::vm::HostedVm::balloon) tells apart from a host
    /// that failed.
    pub fn balloon(&self, amount_mib: u64) -> Result<(), FirecrackerError> {
        self.call("PATCH", "/balloon", &json!({ "amount_mib": amount_mib }))
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), FirecrackerError> {
        let body = body.to_string();
        let mut stream = tilo_files::native::socket::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )?;
        let (status, message) = read_response(&mut stream)?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(FirecrackerError::Api {
                path: path.to_string(),
                status,
                message,
            })
        }
    }
}

fn read_response(stream: &mut impl io::Read) -> io::Result<(u16, String)> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        head.extend_from_slice(&byte);
        if head.len() > 16 * 1024 {
            return Err(io::Error::other("response head too long"));
        }
    }
    let head = String::from_utf8_lossy(&head);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other("no HTTP status"))?;
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; length.min(64 * 1024)];
    stream.read_exact(&mut body)?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Copy every data extent of the sparse `diff` into `base` at the same
/// offset. Returns the bytes copied.
pub fn merge_diff(diff: &Path, base: &Path) -> io::Result<u64> {
    use rustix::fs::SeekFrom;
    use std::os::unix::fs::FileExt as _;

    let from = std::fs::File::open(diff)?;
    let to = std::fs::OpenOptions::new().write(true).open(base)?;
    let size = from.metadata()?.len();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut offset = 0_u64;
    let mut copied = 0_u64;
    while offset < size {
        let data = match rustix::fs::seek(&from, SeekFrom::Data(offset)) {
            Ok(data) => data,
            // No data after `offset`.
            Err(rustix::io::Errno::NXIO) => break,
            Err(error) => return Err(error.into()),
        };
        let hole = rustix::fs::seek(&from, SeekFrom::Hole(data))?;
        let mut at = data;
        while at < hole {
            let want = usize::try_from(hole.saturating_sub(at))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let chunk = buffer.get_mut(..want).unwrap_or_default();
            let read = from.read_at(chunk, at)?;
            if read == 0 {
                break;
            }
            to.write_all_at(chunk.get(..read).unwrap_or_default(), at)?;
            let read = u64::try_from(read).unwrap_or(u64::MAX);
            at = at.saturating_add(read);
            copied = copied.saturating_add(read);
        }
        offset = hole;
    }
    to.sync_data()?;
    Ok(copied)
}

/// Make the VM's disk: a copy of the base image, or a blank ext4
/// filesystem, the size of the VM's disk. A disk only ever grows.
pub fn create_disk(machine: &Machine, disk: &Path, size: VmSize) -> io::Result<()> {
    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = disk.with_extension("raw.partial");
    let _ = std::fs::remove_file(&partial);
    if let Some(base) = &machine.base_disk {
        copy_sparse(base, &partial)?;
        grow_disk(&partial, size)?;
    } else {
        std::fs::File::create(&partial)?.set_len(size.disk_bytes())?;
        let status = std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-E", "lazy_itable_init=1,lazy_journal_init=1"])
            .arg(&partial)
            .status()?;
        if !status.success() {
            return Err(io::Error::other("mkfs.ext4 failed"));
        }
    }
    std::fs::rename(&partial, disk)
}

/// Grow `disk` to `size`'s disk; never shrink it. The guest grows its
/// filesystem into the space at boot.
pub fn grow_disk(disk: &Path, size: VmSize) -> io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(disk)?;
    if file.metadata()?.len() < size.disk_bytes() {
        file.set_len(size.disk_bytes())?;
    }
    Ok(())
}

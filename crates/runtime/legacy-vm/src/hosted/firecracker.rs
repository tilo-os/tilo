//! Firecracker, the hypervisor of hosted VMs: its configuration, its API,
//! and the snapshot files.
//!
//! # Snapshots
//!
//! Every VM runs with dirty-page tracking. Suspending writes a *diff*
//! snapshot: only the pages written since the VM booted or was last
//! restored. The first one after a cold boot is the whole state (a page the
//! guest never wrote is zero), stored as a sparse file the size of the
//! guest's RAM that takes only what the guest touched on disk. Each later
//! diff is merged into that file ([`merge_diff`]) once Firecracker has
//! exited, so the file is always the VM's complete memory.
//!
//! Restoring maps that file privately: the guest's pages are read from it
//! only when touched, so a restored VM costs what it uses, not its size.
//! The file must stay unchanged while the VM runs, and is only valid
//! together with the disk as it was at the snapshot: so the state file is
//! removed while the VM runs ([`SnapshotFiles::in_use`]) and only a
//! completed suspend writes it back. A VM whose state file is missing boots
//! cold from its disk.
//!
//! # Clones
//!
//! A workspace's first wake need not boot: it can start as a clone of a
//! template ([`clone_vm`]), a VM of its size booted once from the guest image
//! and snapshotted with the session's and Chromium's files already in its
//! page cache. The clone's disk is a reflinked copy of the template's, and
//! its memory file *is* the template's (a hard link), so every clone of a
//! template maps the same file privately and the host keeps one copy of what
//! none of them wrote. Its first suspend copies that file before merging
//! into it ([`SnapshotFiles::complete`]), so a clone never writes the
//! template, and the jail never hands a linked file to a VM's uid. The
//! template holds no identity: a workspace's is handed to its session at the
//! hand-off, and Firecracker's VM generation ID has each clone's kernel
//! reseed its randomness when it is restored.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use thiserror::Error;

use super::size::VmSize;

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
    /// A raw ext4 image every VM's disk starts as a copy of (reflinked where
    /// the filesystem can), then grown to the VM's size. Without one, a
    /// fresh disk is a blank ext4 filesystem.
    pub base_disk: Option<PathBuf>,
}

impl Machine {
    pub const DEFAULT_CMDLINE: &'static str = "console=ttyS0 reboot=k panic=1 quiet";
}

/// Where one VM's files are.
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

/// A VM's snapshot on disk.
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
    /// Whether the VM can be restored from here.
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

    /// A cold boot starts from nothing: no memory to merge into.
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

    /// Bytes the snapshot takes on disk.
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

/// What a file takes on disk, holes not counted.
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

/// `firecracker` booting from `config`.
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

/// One VM's Firecracker API.
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

    /// Move the VM's balloon to `amount_mib`: the guest's usable memory
    /// shrinks by that much, which the host gets back as its Firecracker
    /// process gives the pages up. A VM whose Firecracker has no balloon
    /// device answers with a client error, which
    /// [`HostedVm::balloon`](super::vm::HostedVm::balloon) tells apart from a
    /// host that failed.
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

/// The status and body of one HTTP/1.1 response.
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{FileExt as _, PermissionsExt as _};

    use super::*;

    #[test]
    fn a_disk_copied_from_a_read_only_base_is_the_vms_to_write() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("rootfs.ext4");
        std::fs::write(&base, [7_u8; 4096]).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o444)).unwrap();
        let machine = Machine {
            firecracker: dir.path().join("firecracker"),
            kernel: dir.path().join("vmlinux"),
            initramfs: None,
            cmdline: Machine::DEFAULT_CMDLINE.into(),
            base_disk: Some(base),
        };
        let disk = dir.path().join("vmm/disk.raw");
        create_disk(&machine, &disk, VmSize::Small).unwrap();
        let meta = std::fs::metadata(&disk).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            meta.len(),
            VmSize::Small.disk_bytes(),
            "grown to the VM's size"
        );
    }

    #[test]
    fn a_clone_shares_its_templates_memory_until_its_first_suspend() {
        let dir = tempfile::tempdir().unwrap();
        let template = files(&dir.path().join("template"));
        let (from, disk) = (template.snapshot(), template.disk());
        std::fs::create_dir_all(template.vmm()).unwrap();
        std::fs::write(&disk, b"the template's disk").unwrap();
        std::fs::write(&from.state, b"state").unwrap();
        std::fs::write(&from.memory, [1_u8; 8192]).unwrap();
        for path in [&disk, &from.state, &from.memory] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        let clone = files(&dir.path().join("clone"));
        clone_vm(&template, &clone).unwrap();
        let to = clone.snapshot();
        assert!(to.valid(), "the clone wakes by restoring");
        assert_eq!(std::fs::read(clone.disk()).unwrap(), b"the template's disk");
        assert!(
            shared(&to.memory) && shared(&from.memory),
            "one memory file, mapped by both"
        );
        // phase: its first suspend writes its own copy, never the template
        to.in_use().unwrap();
        let diff = std::fs::File::create(&to.diff).unwrap();
        diff.set_len(8192).unwrap();
        diff.write_all_at(&[9; 4096], 4096).unwrap();
        std::fs::write(&to.new_state, b"its state").unwrap();
        to.complete().unwrap();
        assert!(!shared(&to.memory), "the clone's memory is its own now");
        assert_eq!(std::fs::read(&from.memory).unwrap(), [1_u8; 8192]);
        let memory = std::fs::read(&to.memory).unwrap();
        assert_eq!((memory[0], memory[4096]), (1, 9));
    }

    fn files(dir: &Path) -> VmFiles {
        VmFiles {
            state: dir.join("state"),
            run: dir.join("run"),
            jailed: false,
        }
    }

    #[test]
    fn the_configuration_names_the_size_disk_and_vsock() {
        let machine = Machine {
            firecracker: "/usr/bin/firecracker".into(),
            kernel: "/k/vmlinux".into(),
            initramfs: None,
            cmdline: Machine::DEFAULT_CMDLINE.into(),
            base_disk: None,
        };
        let files = files(Path::new("/h/vm"));
        let config: serde_json::Value =
            serde_json::from_str(&config_json(&machine, VmSize::Large, &files, Some(100))).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 8);
        assert_eq!(config["machine-config"]["mem_size_mib"], 16384);
        assert_eq!(config["machine-config"]["track_dirty_pages"], true);
        assert_eq!(
            config["balloon"]["amount_mib"], 0,
            "a VM starts holding its memory, and gives it back under pressure"
        );
        assert_eq!(
            config["balloon"]["deflate_on_oom"], true,
            "a guest the balloon pushed too far gives the memory back itself"
        );
        assert_eq!(
            config["drives"][0]["path_on_host"],
            "/h/vm/state/vmm/disk.raw"
        );
        assert_eq!(config["drives"][0]["is_root_device"], true);
        assert_eq!(config["vsock"]["uds_path"], "/h/vm/run/v.sock");
        assert_eq!(config["network-interfaces"][0]["host_dev_name"], "tap0");
        assert!(
            config["boot-source"]["boot_args"]
                .as_str()
                .unwrap()
                .ends_with(" ip=10.0.2.15::10.0.2.2:255.255.255.0::eth0:off:10.0.2.3")
        );
        assert!(config["boot-source"].get("initrd_path").is_none());
        assert_eq!(
            boot_argv(&machine, &files),
            [
                "/usr/bin/firecracker",
                "--api-sock",
                "/h/vm/run/api.sock",
                "--config-file",
                "/h/vm/state/vmm/firecracker.json"
            ]
        );
        // phase: jailed, Firecracker names everything inside its jail
        let jailed = VmFiles {
            jailed: true,
            ..files
        };
        let config: serde_json::Value =
            serde_json::from_str(&config_json(&machine, VmSize::Small, &jailed, None)).unwrap();
        assert_eq!(config["boot-source"]["kernel_image_path"], "/guest/vmlinux");
        assert_eq!(config["drives"][0]["path_on_host"], "/vm/disk.raw");
        assert_eq!(config["vsock"]["uds_path"], "/s/v.sock");
        assert!(config.get("network-interfaces").is_none());
        assert_eq!(jailed.for_vmm(&jailed.snapshot().diff), "/vm/vm.diff");
    }

    #[test]
    fn diffs_merge_into_the_sparse_memory_file() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = files(dir.path()).snapshot();
        std::fs::create_dir_all(dir.path().join("state/vmm")).unwrap();
        // phase: after a cold boot the first suspend writes the base itself
        {
            assert_eq!(snapshot.next_memory_target(), snapshot.memory);
            let base = std::fs::File::create(&snapshot.memory).unwrap();
            base.set_len(64 << 20).unwrap();
            base.write_all_at(&[1; 4096], 0).unwrap();
            base.write_all_at(&[2; 4096], 32 << 20).unwrap();
            std::fs::write(&snapshot.new_state, b"state-1").unwrap();
            snapshot.complete().unwrap();
            assert!(snapshot.valid());
            assert!(snapshot.bytes_on_disk() < 1 << 20, "holes are not stored");
        }
        // phase: a restored VM's suspend writes a diff that is merged in
        {
            snapshot.in_use().unwrap();
            assert!(!snapshot.valid(), "a running VM's snapshot is stale");
            assert_eq!(snapshot.next_memory_target(), snapshot.diff);
            let diff = std::fs::File::create(&snapshot.diff).unwrap();
            diff.set_len(64 << 20).unwrap();
            diff.write_all_at(&[3; 8192], 32 << 20).unwrap();
            diff.write_all_at(&[4; 4096], 48 << 20).unwrap();
            std::fs::write(&snapshot.new_state, b"state-2").unwrap();
            snapshot.complete().unwrap();
            assert!(snapshot.valid() && !snapshot.diff.exists());
            let memory = std::fs::File::open(&snapshot.memory).unwrap();
            let mut page = [0_u8; 4096];
            for (offset, byte) in [
                (0, 1),
                (32 << 20, 3),
                ((32 << 20) + 4096, 3),
                (48 << 20, 4),
                (16 << 20, 0),
            ] {
                memory.read_exact_at(&mut page, offset).unwrap();
                assert!(page.iter().all(|b| *b == byte), "at {offset}");
            }
            assert_eq!(std::fs::read(&snapshot.state).unwrap(), b"state-2");
        }
        // phase: a cold boot discards it all
        {
            snapshot.discard().unwrap();
            assert!(!snapshot.memory.exists() && !snapshot.state.exists());
        }
    }

    #[test]
    fn api_responses_are_read_to_their_length() {
        let mut ok = &b"HTTP/1.1 204 No Content\r\nServer: Firecracker API\r\n\r\n"[..];
        assert_eq!(read_response(&mut ok).unwrap(), (204, String::new()));
        let body = r#"{"fault_message":"bad"}"#;
        let raw = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            read_response(&mut raw.as_bytes()).unwrap(),
            (400, body.to_string())
        );
    }
}

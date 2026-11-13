//! Each VM in a jail of its own: its own uid, chroot, cgroup and network namespace,
//! so a VM that escapes its guest lands in an unprivileged, empty jail. The service
//! runs unprivileged; a small helper, `tilo-hosted-jail`, runs as root to start the
//! jailer, kill the VM's cgroups and release the jail. Slot `n` runs its VM as
//! `uid_base + 2n` and its network as `uid_base + 2n + 1`, freed when the VM
//! sleeps.

use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::network::Endpoint;
use super::size::VmSize;

pub const DEFAULT_CONFIG: &str = "/etc/tilo/hosted-jail.json";

/// The VM's state directory, inside its jail.
pub const VMM_DIR: &str = "/vm";
/// Its sockets directory, inside its jail.
pub const SOCKETS_DIR: &str = "/s";
/// The guest kernel's directory, inside its jail.
pub const GUEST_DIR: &str = "/guest";

/// The descriptors `start` and `release` take the directories on.
pub const VMM_FD: RawFd = 3;
pub const SOCKETS_FD: RawFd = 4;
pub const GUEST_FD: RawFd = 5;

/// A host's jails: root-owned, read by the helper and the service alike.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailConfig {
    /// The service's uid: the only caller the helper serves, and the owner
    /// of every VM's files while the VM is not running.
    pub service_uid: u32,
    pub service_gid: u32,
    /// Slot `n` runs as `uid_base + 2n` (the VM) and `uid_base + 2n + 1`
    /// (its network). Nothing else on the host may use these uids.
    pub uid_base: u32,
    /// The most VMs awake at once.
    pub slots: u32,
    pub jailer: PathBuf,
    pub firecracker: PathBuf,
    /// Where the jails are: on the same filesystem as nothing the VMs may
    /// reach; root-owned.
    pub chroot_base: PathBuf,
    /// The cgroup (under `/sys/fs/cgroup`) every VM's cgroup is made in.
    pub cgroup: String,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub pasta: PathBuf,
    /// The resolver VMs' DNS goes to (a public one: the host's own may
    /// answer for cluster-internal names).
    pub resolver: Ipv4Addr,
    /// Networks to block besides [`super::network::ALWAYS_BLOCKED`]: the
    /// host's public addresses, cluster networks outside private ranges.
    #[serde(default)]
    pub blocked: Vec<String>,
    /// Each direction, per VM.
    pub bandwidth_mbit: u32,
}

impl JailConfig {
    /// Read `path`, which must be a root-owned file in a root-owned
    /// directory, neither writable by anyone else: whoever can change it
    /// decides what the helper runs as root.
    pub fn load(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt as _;
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        for (what, checked) in [("file", path), ("directory", parent)] {
            let meta = std::fs::symlink_metadata(checked)
                .map_err(|e| format!("{}: {e}", checked.display()))?;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.file_type().is_symlink() {
                return Err(format!(
                    "the jail configuration's {what} {} must be root's and writable only by root",
                    checked.display()
                ));
            }
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(text).map_err(|e| format!("the jail configuration: {e}"))?;
        for (what, path) in [
            ("jailer", &config.jailer),
            ("firecracker", &config.firecracker),
            ("chroot_base", &config.chroot_base),
        ] {
            if !path.is_absolute() {
                return Err(format!("the jail configuration's {what} must be absolute"));
            }
        }
        if config.uid_base == 0
            || config.slots == 0
            || config.slots > 65_536
            || config
                .uid_base
                .checked_add(config.slots.saturating_mul(2))
                .is_none()
        {
            return Err("the jail configuration's uid range is not usable".into());
        }
        let range = config.uid_base
            ..config
                .uid_base
                .saturating_add(config.slots.saturating_mul(2));
        if range.contains(&config.service_uid) || range.contains(&config.service_gid) {
            return Err("the service's uid is inside the VMs' uid range".into());
        }
        if config.cgroup.is_empty()
            || config
                .cgroup
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("the jail configuration's cgroup must be a relative path".into());
        }
        if let Some(network) = &config.network {
            if !network.pasta.is_absolute() {
                return Err("the jail configuration's pasta must be absolute".into());
            }
            config.blocked()?;
        }
        Ok(config)
    }

    pub fn blocked(&self) -> Result<Vec<(Ipv4Addr, u8)>, String> {
        self.network.as_ref().map_or(Ok(Vec::new()), |network| {
            network
                .blocked
                .iter()
                .map(|text| super::network::parse_network(text))
                .collect()
        })
    }

    #[must_use]
    pub fn vm_uid(&self, slot: u32) -> u32 {
        self.uid_base.saturating_add(slot.saturating_mul(2))
    }

    #[must_use]
    pub fn net_uid(&self, slot: u32) -> u32 {
        self.vm_uid(slot).saturating_add(1)
    }

    #[must_use]
    pub fn id(slot: u32) -> String {
        format!("vm-{slot}")
    }

    /// `<chroot_base>/<firecracker's file name>/vm-<slot>`, as the jailer
    /// lays it out; the chroot is its `root`.
    #[must_use]
    pub fn jail_dir(&self, slot: u32) -> PathBuf {
        let exec = self
            .firecracker
            .file_name()
            .map_or_else(|| "firecracker".into(), std::ffi::OsStr::to_os_string);
        self.chroot_base.join(exec).join(Self::id(slot))
    }

    /// Where the slot's `pasta` writes its pid.
    #[must_use]
    pub fn net_dir(&self, slot: u32) -> PathBuf {
        self.chroot_base.join("net").join(Self::id(slot))
    }

    #[must_use]
    pub fn cgroup_dir(&self) -> PathBuf {
        Path::new("/sys/fs/cgroup").join(&self.cgroup)
    }
}

#[derive(Clone, Debug)]
pub struct Helper {
    pub exe: PathBuf,
    pub config_path: PathBuf,
    pub config: JailConfig,
}

/// The directories one VM's jail is made of, opened by the service.
pub struct JailDirs<'a> {
    pub vmm: &'a File,
    pub sockets: &'a File,
    pub guest: Option<&'a File>,
}

impl Helper {
    /// The helper at `exe` with the configuration at `config_path`, as the
    /// service reads it (it is world-readable; the helper re-checks who
    /// owns it).
    pub fn open(exe: PathBuf, config_path: PathBuf) -> Result<Self, String> {
        let config = JailConfig::load(&config_path)?;
        let me = rustix::process::getuid().as_raw();
        if me != config.service_uid {
            return Err(format!(
                "the jail configuration serves uid {}, and this service runs as {me}",
                config.service_uid
            ));
        }
        Ok(Self {
            exe,
            config_path,
            config,
        })
    }

    fn command(&self, verb: &str, slot: u32) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .arg("--config")
            .arg(&self.config_path)
            .arg(verb)
            .arg("--slot")
            .arg(slot.to_string())
            .stdin(Stdio::null());
        command
    }

    /// The command that becomes the slot's Firecracker: booting from
    /// `/vm/firecracker.json` when `boot`, else waiting for a snapshot to
    /// load. `relay` is the one destination besides the internet the VM's
    /// network allows, for its browser session. The caller spawns it and
    /// owns the child.
    #[must_use]
    pub fn start(
        &self,
        slot: u32,
        size: VmSize,
        boot: bool,
        dirs: &JailDirs<'_>,
        relay: Option<Endpoint>,
    ) -> Command {
        let mut command = self.command("start", slot);
        command
            .arg("--vcpus")
            .arg(size.vcpus().to_string())
            .arg("--memory-mib")
            .arg(size.memory_mib().to_string())
            .arg(if boot { "--boot" } else { "--restore" });
        if let Some(relay) = relay {
            command.arg("--relay-endpoint").arg(relay.to_string());
        }
        pass_dirs(&mut command, dirs);
        command
    }

    pub fn stop(&self, slot: u32) -> io::Result<()> {
        run(self.command("stop", slot))
    }

    pub fn release(&self, slot: u32, dirs: &JailDirs<'_>) -> io::Result<()> {
        let mut command = self.command("release", slot);
        pass_dirs(&mut command, dirs);
        run(command)
    }
}

fn run(mut command: Command) -> io::Result<()> {
    let output = command.stderr(Stdio::piped()).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "tilo-hosted-jail: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Give the child `dirs` as [`VMM_FD`], [`SOCKETS_FD`] and [`GUEST_FD`].
fn pass_dirs(command: &mut Command, dirs: &JailDirs<'_>) {
    let fds = [
        Some(dirs.vmm.as_raw_fd()),
        Some(dirs.sockets.as_raw_fd()),
        dirs.guest.map(AsRawFd::as_raw_fd),
    ];
    // SAFETY: `dup` and `dup2` are async-signal-safe. The sources are first
    // moved above every target, so placing one never overwrites another.
    unsafe {
        command.pre_exec(move || {
            let mut moved = [-1; 3];
            for (slot, fd) in moved.iter_mut().zip(fds) {
                if let Some(fd) = fd {
                    *slot = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 16);
                    if *slot < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            for (target, fd) in [VMM_FD, SOCKETS_FD, GUEST_FD].into_iter().zip(moved) {
                if fd >= 0 && libc::dup2(fd, target) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// A host's jails as the service uses them: the helper, and which slots are free.
pub struct Jail {
    pub helper: Helper,
    free: std::sync::Mutex<Vec<u32>>,
}

impl std::fmt::Debug for Jail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jail")
            .field("helper", &self.helper)
            .finish_non_exhaustive()
    }
}

impl Jail {
    #[must_use]
    pub fn new(helper: Helper) -> Self {
        let free = (0..helper.config.slots).rev().collect();
        Self {
            helper,
            free: std::sync::Mutex::new(free),
        }
    }

    fn free(&self) -> std::sync::MutexGuard<'_, Vec<u32>> {
        self.free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn take(&self) -> Option<u32> {
        self.free().pop()
    }

    pub fn give_back(&self, slot: u32) {
        let mut free = self.free();
        if !free.contains(&slot) {
            free.push(slot);
        }
    }

    /// A slot a previous service left a VM in is not free until released.
    pub fn claim(&self, slot: u32) -> bool {
        let mut free = self.free();
        let before = free.len();
        free.retain(|free| *free != slot);
        free.len() != before
    }

    #[must_use]
    pub fn network_mbit(&self) -> Option<u32> {
        self.helper
            .config
            .network
            .as_ref()
            .map(|network| network.bandwidth_mbit)
    }
}

/// `confine-fs` arguments that give a share server namespaces of its own
/// in which `dirs` (the jails, the service's sockets) do not exist and no
/// network does.
#[must_use]
pub fn hide_args(dirs: &[&Path]) -> Vec<String> {
    let mut args = vec!["--isolate".to_string()];
    for dir in dirs {
        args.extend(["--hide".to_string(), dir.display().to_string()]);
    }
    args
}

#[cfg(target_os = "linux")]
pub mod privileged;

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> JailConfig {
        JailConfig::parse(
            r#"{
                "service_uid": 990, "service_gid": 990,
                "uid_base": 200000, "slots": 64,
                "jailer": "/usr/libexec/tilo/jailer",
                "firecracker": "/usr/libexec/tilo/firecracker",
                "chroot_base": "/var/lib/tilo-hosted/jail",
                "cgroup": "tilo-hosted",
                "network": {
                    "pasta": "/usr/bin/pasta",
                    "resolver": "1.1.1.1",
                    "blocked": ["203.0.113.7/32"],
                    "bandwidth_mbit": 200
                }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn each_slot_has_its_own_uids_jail_and_network_dir() {
        let config = config();
        assert_eq!((config.vm_uid(0), config.net_uid(0)), (200_000, 200_001));
        assert_eq!((config.vm_uid(5), config.net_uid(5)), (200_010, 200_011));
        assert_eq!(
            config.jail_dir(5),
            Path::new("/var/lib/tilo-hosted/jail/firecracker/vm-5")
        );
        assert_eq!(
            config.net_dir(5),
            Path::new("/var/lib/tilo-hosted/jail/net/vm-5")
        );
        assert_eq!(config.cgroup_dir(), Path::new("/sys/fs/cgroup/tilo-hosted"));
        assert_eq!(config.blocked().unwrap().len(), 1);
    }

    #[test]
    fn a_configuration_that_would_hand_out_wrong_uids_or_paths_is_refused() {
        let good = serde_json::to_value(config()).unwrap();
        let with = |key: &str, value: serde_json::Value| {
            let mut changed = good.clone();
            changed[key] = value;
            JailConfig::parse(&changed.to_string())
        };
        assert!(with("uid_base", 0.into()).is_err(), "root is no VM's uid");
        assert!(with("service_uid", 200_003.into()).is_err());
        assert!(with("jailer", "jailer".into()).is_err());
        assert!(with("cgroup", "../escape".into()).is_err());
        assert!(with("cgroup", "/abs".into()).is_err());
        assert!(with("slots", 0.into()).is_err());
        let mut network = good.clone();
        network["network"]["blocked"] = serde_json::json!(["not a network"]);
        assert!(JailConfig::parse(&network.to_string()).is_err());
        let mut unknown = good;
        unknown["setuid"] = true.into();
        assert!(JailConfig::parse(&unknown.to_string()).is_err());
    }

    #[test]
    fn only_a_root_owned_configuration_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosted-jail.json");
        std::fs::write(&path, serde_json::to_string(&config()).unwrap()).unwrap();
        if rustix::process::getuid().as_raw() != 0 {
            let refused = JailConfig::load(&path).unwrap_err();
            assert!(refused.contains("must be root's"), "{refused}");
        }
    }
}

//! The VM registry and how the current VM is chosen.
//!
//! `vms.toml` lives in the platform config dir (`~/.config/tilo-vm/` on
//! Linux, `~/Library/Application Support/tilo-vm/` on macOS,
//! `%APPDATA%\tilo-vm\` on Windows); `TILO_VM_CONFIG` overrides the path:
//!
//! ```toml
//! default = "dev"
//! min_free_gb = 5          # builds, swaps and vm start refuse below this
//! warn_disk_gb = 80        # status warns when a VM disk grows past this
//! keep_generations = 5     # gc keeps this many per slot
//! keep_targets = 2         # gc keeps this many in-VM build dirs
//!
//! [remote_builders]        # swap --builder remote, by architecture: a cargo
//! aarch64 = ["remote-cargo", "--on-builder"]   # drop-in whose `run` runs there
//!
//! [vms.dev]
//! hypervisor = "libvirt"   # libvirt | qemu | utm | hyperv | whpx
//! ssh = "root@127.0.0.1:2223"
//! key = "/home/me/.config/tilo-vm/keys/dev_ed25519"   # made by vm create
//! image = "/var/lib/images/tilo-vm.qcow2"
//! disk = "/var/lib/images/dev.qcow2"
//! ```
//!
//! The VM is taken from `TILO_VM`, then `.tilo-vm` in the worktree
//! (`vm = "dev"`, or just the name), then `default` above. `.tilo-vm` may
//! also name the worktree's default slot (`slot = "review-a"`), which
//! `TILO_VM_SLOT` overrides; without either it is named after the branch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_free_gb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_disk_gb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_generations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_targets: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remote_builders: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub vms: BTreeMap<String, VmEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bases: BTreeMap<String, Base>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base {
    pub disk: String,
    /// Its own backing file: the image the VM was created from, or the
    /// base of an earlier branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `raw` when `image` is a raw disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_format: Option<String>,
    pub from: String,
    pub frozen: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmEntry {
    pub hypervisor: String,
    /// `[user@]host[:port]`; when absent the adapter discovers the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Remote command that serves RPC, e.g. `sudo -n tilo-vmd rpc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `raw` when `image` is a raw disk; absent for qcow2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_esp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub emulated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Windows WHPX: omit the display and USB devices for SSH-only VMs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub headless: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heads: Option<u8>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gl: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_img: Option<String>,
    /// Windows ARM64 WHPX: the QEMU binary and its matching EDK2 firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_vars: Option<String>,
    /// A device's hostname: when its address stops answering, tilo-vm
    /// finds it again as `<hostname>.local`. Recorded whenever tilo-vm
    /// reaches it (`vm add`, `vm status`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<crate::reach::WakeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_kernel: Option<DirectKernel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branched_from: Option<String>,
    /// The VM whose workspaces `vm branch` has not yet seen take this
    /// branch in: it re-keys at its first boot, and only a workspace's host
    /// admits a new device (`vm branch --no-admit`, or an admission that
    /// failed). The next `vm branch` of that VM boots it once beside it to
    /// have it taken in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaits_host: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRoute {
    pub device: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_relay: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectKernel {
    pub kernel: String,
    pub initrd: String,
    pub cmdline: String,
    /// WHPX direct boot has no systemd-stub EFI variable. Bind Tilo root
    /// discovery to this VM disk's ESP by its GPT partition UUID instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader_partuuid: Option<String>,
    #[serde(default)]
    pub kver: String,
}

pub const DEFAULT_MIN_FREE_GB: u64 = 5;
pub const DEFAULT_WARN_DISK_GB: u64 = 80;
pub const DEFAULT_KEEP_GENERATIONS: u32 = 5;
pub const WORKTREE_FILE: &str = ".tilo-vm";

pub fn registry_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TILO_VM_CONFIG") {
        return PathBuf::from(path);
    }
    tilo_paths::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tilo-vm")
        .join("vms.toml")
}

pub fn admin_identity_root(registry: &Path) -> PathBuf {
    registry
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("admin-identity")
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self, CliError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| CliError::Config {
                path: path.display().to_string(),
                detail: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CliError::io(format!("read {}", path.display()))(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), CliError> {
        let text = toml::to_string_pretty(self).map_err(|e| CliError::Config {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let context = format!("write {}", path.display());
        #[cfg(windows)]
        {
            std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))
                .map_err(CliError::io(context.clone()))?;
            tilo_files::native::atomic::write_regenerable(path, text.as_bytes())
                .map_err(CliError::io(context))
        }
        #[cfg(not(windows))]
        {
            tilo_files::native::atomic::write_creating_parent(path, text.as_bytes())
                .map_err(CliError::io(context))
        }
    }

    pub fn min_free_bytes(&self) -> u64 {
        self.min_free_gb
            .unwrap_or(DEFAULT_MIN_FREE_GB)
            .saturating_mul(1 << 30)
    }

    pub fn warn_disk_bytes(&self) -> u64 {
        self.warn_disk_gb
            .unwrap_or(DEFAULT_WARN_DISK_GB)
            .saturating_mul(1 << 30)
    }

    pub fn keep_generations(&self) -> u32 {
        self.keep_generations.unwrap_or(DEFAULT_KEEP_GENERATIONS)
    }

    /// What has base `disk` as its backing file: the VMs, then the bases
    /// made from them, by name.
    pub fn base_users(&self, disk: &str) -> Vec<String> {
        let vms = self
            .vms
            .iter()
            .filter(|(_, vm)| vm.image.as_deref() == Some(disk));
        let bases = self
            .bases
            .iter()
            .filter(|(_, base)| base.image.as_deref() == Some(disk));
        vms.map(|(name, _)| name.clone())
            .chain(bases.map(|(id, _)| format!("base {id}")))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Source {
    Argument,
    Env,
    Worktree { file: String },
    RegistryDefault { file: String },
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Self::Argument => "named on the command line".into(),
            Self::Env => "from TILO_VM".into(),
            Self::Worktree { file } => format!("from {file}"),
            Self::RegistryDefault { file } => format!("default in {file}"),
        }
    }
}

pub fn parse_worktree_file(text: &str) -> Option<String> {
    if let Ok(table) = text.parse::<toml::Table>()
        && let Some(vm) = table.get("vm").and_then(toml::Value::as_str)
    {
        return Some(vm.to_owned());
    }
    let bare = text.trim();
    (!bare.is_empty() && !bare.contains(['=', '\n', ' '])).then(|| bare.to_owned())
}

pub fn parse_worktree_slot(text: &str) -> Option<String> {
    let table = text.parse::<toml::Table>().ok()?;
    let slot = table.get("slot")?.as_str()?.trim();
    (!slot.is_empty()).then(|| slot.to_owned())
}

pub fn with_worktree_vm(old: &str, vm: &str) -> String {
    let mut table = old.parse::<toml::Table>().unwrap_or_default();
    table.insert("vm".into(), toml::Value::String(vm.to_owned()));
    toml::to_string(&table).unwrap_or_else(|_| format!("vm = \"{vm}\"\n"))
}

/// Pick the VM: `TILO_VM`, then the worktree file, then the registry default.
pub fn resolve(
    env: Option<&str>,
    worktree_file: Option<(&Path, &str)>,
    registry: &Registry,
    registry_path: &Path,
) -> Option<(String, Source)> {
    if let Some(name) = env.map(str::trim).filter(|n| !n.is_empty()) {
        return Some((name.to_owned(), Source::Env));
    }
    if let Some((path, text)) = worktree_file
        && let Some(name) = parse_worktree_file(text)
    {
        return Some((
            name,
            Source::Worktree {
                file: path.display().to_string(),
            },
        ));
    }
    registry.default.clone().map(|name| {
        (
            name,
            Source::RegistryDefault {
                file: registry_path.display().to_string(),
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        toml::from_str(
            r#"
            default = "shared"
            [vms.shared]
            hypervisor = "libvirt"
            [vms.mine]
            hypervisor = "qemu"
            ssh = "root@127.0.0.1:2222"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn configuration_layers_env_worktree_file_and_registry() {
        {
            let reg = registry();
            let path = Path::new("/cfg/vms.toml");
            let wt = Some((Path::new("/wt/.tilo-vm"), "vm = \"mine\"\n"));
            assert_eq!(
                resolve(Some("env-vm"), wt, &reg, path).unwrap(),
                ("env-vm".into(), Source::Env)
            );
            let (name, source) = resolve(None, wt, &reg, path).unwrap();
            assert_eq!(name, "mine");
            assert!(source.describe().contains("/wt/.tilo-vm"));
            let (name, source) = resolve(Some("  "), None, &reg, path).unwrap();
            assert_eq!(name, "shared");
            assert_eq!(
                source,
                Source::RegistryDefault {
                    file: "/cfg/vms.toml".into()
                }
            );
            assert!(resolve(None, None, &Registry::default(), path).is_none());
        }

        {
            assert_eq!(parse_worktree_file("vm = \"dev\"").as_deref(), Some("dev"));
            assert_eq!(parse_worktree_file("dev\n").as_deref(), Some("dev"));
            assert_eq!(parse_worktree_file("").as_deref(), None);
            assert_eq!(parse_worktree_file("other = 1").as_deref(), None);
        }

        {
            let text = "vm = \"dev\"\nslot = \"ux1\"\n";
            assert_eq!(parse_worktree_slot(text).as_deref(), Some("ux1"));
            assert_eq!(parse_worktree_slot("vm = \"dev\"").as_deref(), None);
            assert_eq!(parse_worktree_slot("dev").as_deref(), None);
            let rewritten = with_worktree_vm(text, "shared");
            assert_eq!(parse_worktree_file(&rewritten).as_deref(), Some("shared"));
            assert_eq!(parse_worktree_slot(&rewritten).as_deref(), Some("ux1"));
            let bare = with_worktree_vm("dev\n", "shared");
            assert_eq!(parse_worktree_file(&bare).as_deref(), Some("shared"));
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sub/vms.toml");
            assert_eq!(Registry::load(&path).unwrap(), Registry::default());
            let reg = registry();
            reg.save(&path).unwrap();
            assert_eq!(Registry::load(&path).unwrap(), reg);
            assert_eq!(reg.keep_generations(), DEFAULT_KEEP_GENERATIONS);
            assert_eq!(reg.min_free_bytes(), 5 << 30);
            std::fs::write(&path, "vms = 3").unwrap();
            assert!(matches!(
                Registry::load(&path),
                Err(CliError::Config { .. })
            ));
        }
    }
}

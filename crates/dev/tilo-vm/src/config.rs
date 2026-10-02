//! The VM registry and how the current VM is chosen.
//!
//! `vms.toml` lives in the platform config dir (`~/.config/tilo-vm/` on
//! Linux, `~/Library/Application Support/tilo-vm/` on macOS,
//! `%APPDATA%\tilo-vm\` on Windows); `TILO_VM_CONFIG` overrides the path.
//! A human edits it to tell agents which VM to use:
//!
//! ```toml
//! default = "dev"
//! min_free_gb = 5          # builds, swaps and vm start refuse below this
//! warn_disk_gb = 80        # status warns when a VM disk grows past this
//! keep_generations = 5     # gc keeps this many per slot
//! keep_targets = 2         # gc keeps this many in-VM build dirs
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
    /// In-VM build dirs gc keeps (the daemon's default is 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_targets: Option<u32>,
    #[serde(default)]
    pub vms: BTreeMap<String, VmEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmEntry {
    pub hypervisor: String,
    /// `[user@]host[:port]`; when absent the adapter discovers the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    /// SSH identity file; `vm create` makes one per VM in `keys/` beside
    /// this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Remote command that serves RPC, e.g. `sudo -n tilo-vmd rpc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<String>,
    /// Published dev image the VM disk is created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `raw` when `image` is a raw disk; absent for qcow2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_format: Option<String>,
    /// A raw ESP disk attached read-only ahead of the VM's disk, so the VM
    /// boots its loader and UKI instead of the disk's own (libvirt, whpx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_esp: Option<String>,
    /// The VM's own disk (a copy-on-write child of `image` where supported).
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
    /// libvirt: virtio-gpu outputs (default one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heads: Option<u8>,
    /// libvirt: virgl 3D acceleration, rendered on the host GPU through an
    /// `egl-headless` display beside the SPICE console.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gl: bool,
    /// libvirt: the first output's preferred mode, `WIDTHxHEIGHT`; the others
    /// offer the standard modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Host port forwarded to the guest's SSH port (libvirt, qemu).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,
    /// libvirt connection URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// UTM VM to clone from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The `qemu-img` that creates the VM's disk, resolved when the VM was
    /// created: the one beside the adapter's own tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_img: Option<String>,
    /// Windows ARM64 WHPX: the QEMU binary and its matching EDK2 firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_vars: Option<String>,
    /// A host-built kernel the VM boots directly instead of its disk's boot
    /// loader (`tilo-vm swap kernel --direct` on libvirt, or a manually
    /// prepared development bundle on WHPX). Cleared by
    /// `tilo-vm rollback --slot vm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_kernel: Option<DirectKernel>,
}

/// Direct kernel boot: files on the host, and the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectKernel {
    pub kernel: String,
    pub initrd: String,
    pub cmdline: String,
    /// WHPX direct boot has no systemd-stub EFI variable. Bind Tilo root
    /// discovery to this VM disk's ESP by its GPT partition UUID instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader_partuuid: Option<String>,
    /// `uname -r` the kernel reports.
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
    xdg_paths::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tilo-vm")
        .join("vms.toml")
}

impl Registry {
    /// # Errors
    /// Unreadable or invalid files. A missing file is an empty registry.
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

    /// # Errors
    /// Write failures.
    pub fn save(&self, path: &Path) -> Result<(), CliError> {
        let text = toml::to_string_pretty(self).map_err(|e| CliError::Config {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let context = format!("write {}", path.display());
        #[cfg(windows)]
        {
            // This host's NTFS rejects the native POSIX rename with ERROR_INVALID_PARAMETER.
            // The VM registry is recoverable development state; a sibling-file
            // rename still avoids exposing a partially written TOML document.
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
}

/// Where the VM choice came from, so `status` can explain it.
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

/// The contents of a worktree's `.tilo-vm`: TOML with `vm = "..."`, or a bare name.
pub fn parse_worktree_file(text: &str) -> Option<String> {
    if let Ok(table) = text.parse::<toml::Table>()
        && let Some(vm) = table.get("vm").and_then(toml::Value::as_str)
    {
        return Some(vm.to_owned());
    }
    let bare = text.trim();
    (!bare.is_empty() && !bare.contains(['=', '\n', ' '])).then(|| bare.to_owned())
}

/// The default slot a worktree file names: `slot = "<name>"`.
pub fn parse_worktree_slot(text: &str) -> Option<String> {
    let table = text.parse::<toml::Table>().ok()?;
    let slot = table.get("slot")?.as_str()?.trim();
    (!slot.is_empty()).then(|| slot.to_owned())
}

/// The worktree file `old` with its VM set to `vm`, keeping its other keys.
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

    /// Env beats the worktree file beats the default; the worktree file accepts TOML or a bare name and can
    /// name the default slot, which vm use keeps; the registry round-trips and has defaults.
    #[test]
    fn configuration_layers_env_worktree_file_and_registry() {
        // Phase: env_beats_worktree_beats_default.
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

        // Phase: worktree_file_accepts_toml_or_a_bare_name.
        {
            assert_eq!(parse_worktree_file("vm = \"dev\"").as_deref(), Some("dev"));
            assert_eq!(parse_worktree_file("dev\n").as_deref(), Some("dev"));
            assert_eq!(parse_worktree_file("").as_deref(), None);
            assert_eq!(parse_worktree_file("other = 1").as_deref(), None);
        }

        // Phase: worktree_file_can_name_the_default_slot_and_vm_use_keeps_it.
        {
            let text = "vm = \"dev\"\nslot = \"ux1\"\n";
            assert_eq!(parse_worktree_slot(text).as_deref(), Some("ux1"));
            assert_eq!(parse_worktree_slot("vm = \"dev\"").as_deref(), None);
            assert_eq!(parse_worktree_slot("dev").as_deref(), None);
            let rewritten = with_worktree_vm(text, "shared");
            assert_eq!(parse_worktree_file(&rewritten).as_deref(), Some("shared"));
            assert_eq!(parse_worktree_slot(&rewritten).as_deref(), Some("ux1"));
            // A bare name is replaced by the table form.
            let bare = with_worktree_vm("dev\n", "shared");
            assert_eq!(parse_worktree_file(&bare).as_deref(), Some("shared"));
        }

        // Phase: registry_round_trips_and_has_defaults.
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

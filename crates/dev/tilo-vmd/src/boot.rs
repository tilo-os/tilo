//! Boot entries for kernels in the `vm` slot.
//!
//! A kernel layer carries `usr/lib/modules/<release>/` with `vmlinuz` and the
//! signed modules. The modules merge into `/usr` with the rest of the VM-wide
//! layers (systemd-sysext), but the boot loader starts the kernel before any
//! extension merges, so `vmlinuz` and an initramfs also go to `/boot` with a
//! boot loader entry of their own (`tilo-vm-<release>-<layer>.conf`):
//!
//! * the initramfs is built in the VM, by the image's own dracut, against the
//!   merged modules, with the arguments os/Containerfile builds the image's
//!   initramfs with ([`IMAGE_DRACUT_ARGS`]);
//! * the entry takes the running kernel's command line, so it boots the same
//!   deployment;
//! * it is the next boot only (`bootctl set-oneshot`, `grub2-reboot`), and
//!   becomes the default once the client confirms the VM came up on it
//!   ([`Boot::confirm`]). A kernel that never comes up is left behind by a
//!   reset: the previous default boots.
//!
//! Putting the image's kernel back restores the default entry tilo-vm found
//! and removes every entry it wrote.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::{BootChange, BootConfirmResult};

use crate::error::DaemonError;
use crate::fsutil::write_atomic;

/// Entry files and kernel directories tilo-vm owns start with this.
pub const ENTRY_PREFIX: &str = "tilo-vm-";
/// Under `/boot`: one directory per installed kernel entry.
pub const KERNEL_DIR: &str = "tilo-vm";

/// The arguments os/Containerfile runs dracut with for the image's initramfs
/// (besides `--force -v --kver <release> <output>`).
/// `tools/policy/check_kernel_dev_build.py` fails when the two differ.
pub const IMAGE_DRACUT_ARGS: &[&str] = &[
    "--reproducible",
    "--compress",
    "zstd",
    "--omit",
    "network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh",
    "--omit-drivers",
    "xfs",
    "--add-drivers",
    "ext4",
    "--add",
    "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback",
];

/// Loaded in the initramfs of every kernel tilo-vmd installs: systemd-sysext
/// needs overlayfs to merge the layer that carries the kernel's modules, so
/// with overlayfs modular (Tilo's config) it has to be loaded before that
/// layer exists. The Tilo image's own initramfs loads it for composefs; an
/// image that boots without composefs would not.
pub const KERNEL_LAYER_DRACUT_ARGS: &[&str] = &["--force-drivers", "overlay"];

/// Written by that dracut step; its presence says the image is one whose
/// initramfs [`IMAGE_DRACUT_ARGS`] describe. Other images (a stand-in
/// distribution) get their distribution's dracut defaults.
pub const IMAGE_INITRAMFS_MARKER: &str = "/usr/lib/tilo/initramfs-modules.txt";

/// Makes udev and modules-load look again once the VM-wide layers (and so
/// the swapped kernel's modules) have merged: until then, `/usr/lib/modules/
/// <release>` does not exist, and a driver requested earlier in boot (the
/// virtio NIC, vkms, uinput) would stay unloaded.
pub const SYSEXT_DROPIN: &str =
    "/etc/systemd/system/systemd-sysext.service.d/50-tilo-vm-kernel.conf";
const SYSEXT_DROPIN_TEXT: &str = "# Written by tilo-vmd: the vm slot carries a kernel whose modules merge with\n\
# systemd-sysext. Load what was requested before they were there. Do not edit.\n\
[Service]\n\
ExecStartPost=-/usr/bin/udevadm trigger --action=add\n\
ExecStartPost=-/usr/bin/systemctl restart --no-block systemd-modules-load.service\n";

const LOADER_INFO_EFIVAR: &str =
    "/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// The boot loaders tilo-vmd can point at an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loader {
    /// systemd-boot, the Tilo image's loader; entry ids end in `.conf`.
    SystemdBoot,
    /// GRUB with BLS entries (`grub2-editenv`); entry ids are the file stem.
    Grub,
}

impl Loader {
    pub fn name(self) -> &'static str {
        match self {
            Self::SystemdBoot => "systemd-boot",
            Self::Grub => "grub",
        }
    }

    /// The id the loader's tools use for the entry file `<stem>.conf`.
    pub fn entry_id(self, stem: &str) -> String {
        match self {
            Self::SystemdBoot => format!("{stem}.conf"),
            Self::Grub => stem.to_owned(),
        }
    }

    /// The entry file stem of a loader id.
    pub fn stem(id: &str) -> &str {
        id.strip_suffix(".conf").unwrap_or(id)
    }
}

/// What the last swap or rollback asked the next boot to run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The entry it booted once (a new kernel) or made the default (the image's).
    pub entry: Option<String>,
    /// `None`: the image's own kernel.
    pub kver: Option<String>,
}

/// `<root>/boot/state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootState {
    /// The default entry before tilo-vm first changed it.
    #[serde(default)]
    pub image_default: Option<String>,
    #[serde(default)]
    pub pending: Option<Pending>,
}

/// The kernel release and module directory a kernel layer carries: exactly
/// one `usr/lib/modules/<release>/` with a `vmlinuz`.
///
/// # Errors
/// [`DaemonError::KernelLayer`] for anything else.
pub fn kernel_in_layer(layer: &Path) -> Result<(String, PathBuf), DaemonError> {
    let modules = layer.join("usr/lib/modules");
    let mut found = Vec::new();
    for entry in fs::read_dir(&modules)
        .map_err(|_| DaemonError::KernelLayer("it has no usr/lib/modules/".into()))?
        .flatten()
    {
        let path = entry.path();
        if path.join("vmlinuz").is_file() {
            found.push((entry.file_name().to_string_lossy().into_owned(), path));
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(DaemonError::KernelLayer(
            "no usr/lib/modules/<release>/vmlinuz".into(),
        )),
        n => Err(DaemonError::KernelLayer(format!(
            "{n} kernels under usr/lib/modules/; a kernel layer carries one"
        ))),
    }
}

/// `tilo-vm-<release>-<first 12 of the layer hash>`: rebuilds keep the
/// release, so the layer tells two builds (and their entries) apart.
pub fn entry_stem(kver: &str, layer_hash: &str) -> String {
    format!(
        "{ENTRY_PREFIX}{kver}-{}",
        layer_hash.get(..12).unwrap_or(layer_hash)
    )
}

/// The running kernel's command line as entry options: the loader adds
/// `BOOT_IMAGE=` and `initrd=` itself.
pub fn boot_options(cmdline: &str) -> String {
    cmdline
        .split_whitespace()
        .filter(|word| !word.starts_with("BOOT_IMAGE=") && !word.starts_with("initrd="))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One BLS type #1 entry.
pub fn entry_text(kver: &str, linux: &str, initrd: &str, options: &str) -> String {
    format!(
        "# Written by tilo-vmd for a kernel in the vm slot (tilo-vm swap kernel). Do not edit.\n\
         title Tilo dev kernel {kver}\n\
         version {kver}\n\
         linux {linux}\n\
         initrd {initrd}\n\
         options {options}\n"
    )
}

/// The value of `key` in a BLS entry.
pub fn entry_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once(char::is_whitespace)?;
        (k == key).then(|| v.trim())
    })
}

/// Whether entry paths are relative to `/boot` (a separate boot partition, or
/// the ESP mounted there) or absolute on the root filesystem, judged from an
/// existing entry whose kernel exists. Returns the prefix to put before
/// `/tilo-vm/...`: `""` or `"/boot"`.
pub fn path_prefix(boot: &Path, entries: &[String]) -> &'static str {
    for text in entries {
        let Some(linux) = entry_field(text, "linux") else {
            continue;
        };
        let relative = linux.trim_start_matches('/');
        if boot.join(relative).is_file() {
            return "";
        }
        if let Some(rest) = relative.strip_prefix("boot/")
            && boot.join(rest).is_file()
        {
            return "/boot";
        }
    }
    ""
}

/// The stem of the entry the running kernel was booted from, matched by the
/// `BOOT_IMAGE=` path GRUB puts on the command line (`(hd0,gpt3)/boot/vmlinuz-…`)
/// against each entry's `linux` path.
pub fn running_entry(cmdline: &str, entries: &[(String, String)], boot: &Path) -> Option<String> {
    let image = cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("BOOT_IMAGE="))?;
    let image = image.rsplit_once(')').map_or(image, |(_, path)| path);
    let normal = |path: &str| {
        let path = path.trim_start_matches('/');
        path.strip_prefix("boot/")
            .filter(|rest| boot.join(rest).exists())
            .unwrap_or(path)
            .to_owned()
    };
    let wanted = normal(image);
    entries
        .iter()
        .find(|(_, text)| entry_field(text, "linux").is_some_and(|l| normal(l) == wanted))
        .map(|(stem, _)| stem.clone())
}

/// Parse `bootctl list --json=short` for the default entry's id.
pub fn systemd_boot_default(json: &str) -> Option<String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    entries
        .iter()
        .find(|e| e.get("isDefault").and_then(serde_json::Value::as_bool) == Some(true))
        .and_then(|e| e.get("id")?.as_str().map(str::to_owned))
}

/// Parse `grub2-editenv - list` for `saved_entry`.
pub fn grub_default(env: &str) -> Option<String> {
    env.lines()
        .find_map(|line| line.strip_prefix("saved_entry="))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Tilo-vm entries to delete when `keep` (entry stems) stay: every other
/// `tilo-vm-*.conf` stem.
pub fn stale_stems(existing: &[String], keep: &[&str]) -> Vec<String> {
    existing
        .iter()
        .filter(|stem| stem.starts_with(ENTRY_PREFIX) && !keep.contains(&stem.as_str()))
        .cloned()
        .collect()
}

/// Boot entries on the running system.
pub struct Boot {
    /// `/boot`.
    pub boot_dir: PathBuf,
    /// `<daemon root>/boot/state.json`.
    pub state_path: PathBuf,
}

fn run(step: &str, command: &mut Command) -> Result<String, DaemonError> {
    let output = command.output().map_err(|e| DaemonError::BootLoader {
        detail: format!("{step}: {e}"),
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(DaemonError::BootLoader {
            detail: format!(
                "{step}: {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

fn on_path(tool: &str) -> bool {
    crate::daemon::on_path(tool)
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

impl Boot {
    pub fn new(root: &Path) -> Self {
        Self {
            boot_dir: PathBuf::from("/boot"),
            state_path: root.join("boot").join("state.json"),
        }
    }

    fn entries_dir(&self) -> PathBuf {
        self.boot_dir.join("loader/entries")
    }

    fn state(&self) -> BootState {
        fs::read(&self.state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &BootState) -> Result<(), DaemonError> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        write_atomic(&self.state_path, &bytes)
    }

    /// # Errors
    /// Neither loader is present.
    pub fn loader(&self) -> Result<Loader, DaemonError> {
        // GRUB sets LoaderInfo too (its `bli` module), so read who wrote it.
        let info = fs::read(LOADER_INFO_EFIVAR)
            .map(|bytes| efivar_string(&bytes))
            .unwrap_or_default();
        if info.starts_with("systemd-boot") && on_path("bootctl") {
            Ok(Loader::SystemdBoot)
        } else if on_path("grub2-editenv") {
            Ok(Loader::Grub)
        } else {
            Err(DaemonError::BootLoader {
                detail: "neither systemd-boot (bootctl and its EFI variables) nor GRUB (grub2-editenv) is here".into(),
            })
        }
    }

    fn default_entry(loader: Loader) -> Option<String> {
        match loader {
            Loader::SystemdBoot => {
                let json = run(
                    "bootctl list",
                    Command::new("bootctl").args(["list", "--json=short", "--no-pager"]),
                )
                .ok()?;
                systemd_boot_default(&json)
            }
            Loader::Grub => grub_default(
                &run(
                    "grub2-editenv list",
                    Command::new("grub2-editenv").args(["-", "list"]),
                )
                .ok()?,
            ),
        }
    }

    fn set_default(loader: Loader, id: &str) -> Result<(), DaemonError> {
        match loader {
            Loader::SystemdBoot => run(
                "bootctl set-default",
                Command::new("bootctl").args(["set-default", id]),
            ),
            Loader::Grub => run(
                "grub2-set-default",
                Command::new("grub2-set-default").arg(id),
            ),
        }
        .map(drop)
    }

    fn set_oneshot(loader: Loader, id: &str) -> Result<(), DaemonError> {
        match loader {
            Loader::SystemdBoot => run(
                "bootctl set-oneshot",
                Command::new("bootctl").args(["set-oneshot", id]),
            ),
            Loader::Grub => run("grub2-reboot", Command::new("grub2-reboot").arg(id)),
        }
        .map(drop)
    }

    /// `(stem, text)` of every entry file.
    fn entries(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = fs::read_dir(self.entries_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".conf")?.to_owned();
                Some((stem, fs::read_to_string(e.path()).ok()?))
            })
            .collect();
        out.sort();
        out
    }

    fn remove_entry(&self, stem: &str) -> Result<(), DaemonError> {
        let file = self.entries_dir().join(format!("{stem}.conf"));
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(DaemonError::io(format!("remove {}", file.display()))(e)),
        }
        let dir = self.boot_dir.join(KERNEL_DIR).join(stem);
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(DaemonError::io(format!("remove {}", dir.display())))?;
        }
        Ok(())
    }

    /// Install the kernel in `layer` (its modules already merged into `/usr`)
    /// as the next boot entry; the current default stays the default.
    ///
    /// # Errors
    /// A layer without a kernel, no supported loader, dracut or I/O failures.
    pub fn install(&self, layer: &Path) -> Result<(BootChange, Vec<String>), DaemonError> {
        let (kver, moddir) = kernel_in_layer(layer)?;
        let hash = layer
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let loader = self.loader()?;
        if !self.entries_dir().is_dir() {
            return Err(DaemonError::BootLoader {
                detail: format!("{} does not exist", self.entries_dir().display()),
            });
        }
        let merged = Path::new("/usr/lib/modules").join(&kver);
        if !merged.join("modules.dep").is_file() {
            return Err(DaemonError::KernelLayer(format!(
                "{} is not in /usr after systemd-sysext refresh",
                merged.display()
            )));
        }
        let stem = entry_stem(&kver, &hash);
        let id = loader.entry_id(&stem);
        let current = self.pinned_default(loader)?;
        let mut state = self.state();
        if state.image_default.is_none()
            && let Some(current) = &current
            && !Loader::stem(current).starts_with(ENTRY_PREFIX)
        {
            state.image_default = Some(current.clone());
        }

        let dir = self.boot_dir.join(KERNEL_DIR).join(&stem);
        fs::create_dir_all(&dir).map_err(DaemonError::io(format!("create {}", dir.display())))?;
        let initramfs = dir.join("initramfs.img");
        let (how, seconds) = if initramfs.is_file() {
            (None, 0.0)
        } else {
            let started = Instant::now();
            let how = build_initramfs(&kver, &initramfs)?;
            (Some(how), started.elapsed().as_secs_f64())
        };
        let vmlinuz = fs::read(moddir.join("vmlinuz"))
            .map_err(DaemonError::io("read the layer's vmlinuz"))?;
        write_atomic(&dir.join("vmlinuz"), &vmlinuz)?;

        let others: Vec<String> = self
            .entries()
            .into_iter()
            .filter(|(s, _)| !s.starts_with(ENTRY_PREFIX))
            .map(|(_, text)| text)
            .collect();
        let prefix = path_prefix(&self.boot_dir, &others);
        let options = boot_options(&read_trimmed("/proc/cmdline"));
        let text = entry_text(
            &kver,
            &format!("{prefix}/{KERNEL_DIR}/{stem}/vmlinuz"),
            &format!("{prefix}/{KERNEL_DIR}/{stem}/initramfs.img"),
            &options,
        );
        write_atomic(
            &self.entries_dir().join(format!("{stem}.conf")),
            text.as_bytes(),
        )?;
        write_atomic(Path::new(SYSEXT_DROPIN), SYSEXT_DROPIN_TEXT.as_bytes())?;
        let _ = Command::new("systemctl").arg("daemon-reload").status();
        Self::set_oneshot(loader, &id)?;

        // Keep the new entry and the fallback; older builds go.
        let fallback_stem = current.as_deref().map(Loader::stem);
        let existing: Vec<String> = self.entries().into_iter().map(|(s, _)| s).collect();
        let keep: Vec<&str> = [Some(stem.as_str()), fallback_stem]
            .into_iter()
            .flatten()
            .collect();
        for old in stale_stems(&existing, &keep) {
            self.remove_entry(&old)?;
        }

        state.pending = Some(Pending {
            entry: Some(id.clone()),
            kver: Some(kver.clone()),
        });
        self.save(&state)?;
        let warnings = install_warnings(loader);
        Ok((
            BootChange {
                kver: Some(kver),
                entry: Some(id),
                fallback: current,
                loader: loader.name().into(),
                initramfs: how,
                initramfs_seconds: seconds,
            },
            warnings,
        ))
    }

    /// The default entry, pinned first when there is none: GRUB then boots
    /// the entry that sorts first, which a newer kernel release would take
    /// over, so the running entry becomes the explicit default and the new
    /// one is the next boot only.
    fn pinned_default(&self, loader: Loader) -> Result<Option<String>, DaemonError> {
        if let Some(current) = Self::default_entry(loader) {
            return Ok(Some(current));
        }
        let running = running_entry(
            &read_trimmed("/proc/cmdline"),
            &self.entries(),
            &self.boot_dir,
        );
        match running {
            Some(running) => {
                let running = loader.entry_id(&running);
                Self::set_default(loader, &running)?;
                Ok(Some(running))
            }
            None => Ok(None),
        }
    }

    /// Put the image's own kernel back: its default entry returns and every
    /// tilo-vm entry goes.
    ///
    /// # Errors
    /// No supported loader, or I/O failures.
    pub fn restore_image(&self) -> Result<BootChange, DaemonError> {
        let loader = self.loader()?;
        let mut state = self.state();
        let entries = self.entries();
        let current = Self::default_entry(loader);
        let target = state
            .image_default
            .clone()
            .or_else(|| {
                current
                    .clone()
                    .filter(|c| !Loader::stem(c).starts_with(ENTRY_PREFIX))
            })
            .or_else(|| {
                entries
                    .iter()
                    .find(|(s, _)| !s.starts_with(ENTRY_PREFIX))
                    .map(|(s, _)| loader.entry_id(s))
            });
        if let Some(target) = &target
            && current.as_ref() != Some(target)
        {
            Self::set_default(loader, target)?;
        }
        for (stem, _) in &entries {
            if stem.starts_with(ENTRY_PREFIX) {
                self.remove_entry(stem)?;
            }
        }
        match fs::remove_file(SYSEXT_DROPIN) {
            Ok(()) => {
                let _ = Command::new("systemctl").arg("daemon-reload").status();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(DaemonError::io(format!("remove {SYSEXT_DROPIN}"))(e)),
        }
        state.image_default = None;
        state.pending = Some(Pending {
            entry: target.clone(),
            kver: None,
        });
        self.save(&state)?;
        Ok(BootChange {
            kver: None,
            entry: target,
            fallback: None,
            loader: loader.name().into(),
            initramfs: None,
            initramfs_seconds: 0.0,
        })
    }

    /// After the reboot: is the expected kernel running? Then its entry
    /// becomes the default.
    ///
    /// # Errors
    /// No supported loader, or the default cannot be set.
    pub fn confirm(&self, expect_version: Option<&str>) -> Result<BootConfirmResult, DaemonError> {
        let running = read_trimmed("/proc/sys/kernel/osrelease");
        let version = read_trimmed("/proc/sys/kernel/version");
        let loader = self.loader()?;
        let mut state = self.state();
        let pending = state.pending.clone();
        let (expected, confirmed) = match &pending {
            Some(Pending {
                entry: Some(entry),
                kver: Some(kver),
            }) => {
                let ok = *kver == running && expect_version.is_none_or(|v| v == version);
                if ok {
                    Self::set_default(loader, entry)?;
                    if loader == Loader::Grub {
                        // GRUB may not have been able to clear it itself.
                        let _ = run(
                            "grub2-editenv unset next_entry",
                            Command::new("grub2-editenv").args(["-", "unset", "next_entry"]),
                        );
                    }
                    state.pending = None;
                    self.save(&state)?;
                }
                (Some(kver.clone()), ok)
            }
            Some(Pending { kver: None, .. }) => {
                // The image's kernel: its default was set before the reboot.
                let ok = !self.entries().iter().any(|(s, text)| {
                    s.starts_with(ENTRY_PREFIX) && entry_field(text, "version") == Some(&running)
                });
                if ok {
                    state.pending = None;
                    self.save(&state)?;
                }
                (None, ok)
            }
            _ => (None, false),
        };
        Ok(BootConfirmResult {
            running,
            version,
            expected,
            confirmed,
            default_entry: Self::default_entry(loader),
        })
    }
}

/// What can keep an installed kernel from booting as intended.
fn install_warnings(loader: Loader) -> Vec<String> {
    let mut warnings = Vec::new();
    if loader == Loader::Grub && grubenv_on_btrfs() {
        warnings.push(
            "GRUB keeps its environment on btrfs here and cannot write it at boot, so the new entry stays the next boot until tilo-vm confirms it: a kernel that does not come up will not fall back by itself".into(),
        );
    }
    let enabled = Command::new("systemctl")
        .args(["is-enabled", "systemd-sysext.service"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled");
    if !enabled {
        warnings.push(
            "systemd-sysext.service is not enabled, so the kernel's modules will not merge at boot: systemctl enable systemd-sysext.service"
                .into(),
        );
    }
    warnings
}

/// Whether GRUB's environment file sits on btrfs without an `env_block`
/// in the filesystem's reserved area: GRUB then cannot write it at boot,
/// so it cannot clear `next_entry`.
fn grubenv_on_btrfs() -> bool {
    let on_btrfs = Command::new("stat")
        .args(["-f", "-c", "%T", "/boot/grub2/grubenv"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "btrfs");
    on_btrfs
        && !fs::read_to_string("/boot/grub2/grubenv")
            .is_ok_and(|env| env.lines().any(|l| l.starts_with("env_block=")))
}

/// An EFI variable's UTF-16LE string value, after its 4-byte attributes.
pub fn efivar_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .get(4..)
        .unwrap_or_default()
        .chunks_exact(2)
        .filter_map(|pair| <[u8; 2]>::try_from(pair).ok().map(u16::from_le_bytes))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Build `output` for `kver` with the image's dracut arguments (or the
/// distribution's defaults on an image without them); says which.
fn build_initramfs(kver: &str, output: &Path) -> Result<String, DaemonError> {
    let temporary = output.with_extension("img.tmp");
    let mut command = Command::new("dracut");
    command.args(["--force", "--quiet"]);
    let how = if Path::new(IMAGE_INITRAMFS_MARKER).is_file() {
        command.args(IMAGE_DRACUT_ARGS);
        "dracut with the image's arguments (os/Containerfile), overlay loaded early"
    } else {
        "dracut with the distribution's defaults (not a Tilo image), overlay loaded early"
    };
    command.args(KERNEL_LAYER_DRACUT_ARGS);
    command.args(["--kver", kver]).arg(&temporary);
    let result = command.output().map_err(|e| DaemonError::Initramfs {
        kver: kver.to_owned(),
        detail: e.to_string(),
    })?;
    if !result.status.success() {
        let _ = fs::remove_file(&temporary);
        let stderr = String::from_utf8_lossy(&result.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines
            .get(lines.len().saturating_sub(15)..)
            .unwrap_or_default();
        return Err(DaemonError::Initramfs {
            kver: kver.to_owned(),
            detail: format!("{}: {}", result.status, tail.join("\n")),
        });
    }
    fs::rename(&temporary, output)
        .map_err(DaemonError::io(format!("move {}", output.display())))?;
    Ok(how.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn boot_entries_carry_one_kernel_release_and_the_running_command_line() {
        // Phase `a_kernel_layer_carries_one_release`.
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(matches!(
                kernel_in_layer(dir.path()),
                Err(DaemonError::KernelLayer(_))
            ));
            let modules = dir.path().join("usr/lib/modules/7.2.5-200.fc44.x86_64+dev");
            fs::create_dir_all(&modules).unwrap();
            assert!(kernel_in_layer(dir.path()).is_err());
            fs::write(modules.join("vmlinuz"), b"k").unwrap();
            let (kver, path) = kernel_in_layer(dir.path()).unwrap();
            assert_eq!(kver, "7.2.5-200.fc44.x86_64+dev");
            assert_eq!(path, modules);
            let other = dir.path().join("usr/lib/modules/6.1/");
            fs::create_dir_all(&other).unwrap();
            fs::write(other.join("vmlinuz"), b"k").unwrap();
            let error = kernel_in_layer(dir.path()).unwrap_err();
            assert!(error.to_string().contains("2 kernels"), "{error}");
        }
        // Phase `entries_take_the_running_command_line_without_loader_words`.
        {
            assert_eq!(
                boot_options(
                    "BOOT_IMAGE=(hd0,gpt3)/vmlinuz-6.1 root=UUID=abc ro rootflags=subvol=root initrd=/x console=ttyS0"
                ),
                "root=UUID=abc ro rootflags=subvol=root console=ttyS0"
            );
            let stem = entry_stem("7.2.5+dev", "0123456789abcdef0123");
            assert_eq!(stem, "tilo-vm-7.2.5+dev-0123456789ab");
            let text = entry_text(
                "7.2.5+dev",
                "/tilo-vm/s/vmlinuz",
                "/tilo-vm/s/initramfs.img",
                "root=x rw",
            );
            assert_eq!(entry_field(&text, "linux"), Some("/tilo-vm/s/vmlinuz"));
            assert_eq!(entry_field(&text, "options"), Some("root=x rw"));
            assert_eq!(entry_field(&text, "version"), Some("7.2.5+dev"));
        }
        // Phase `entry_paths_follow_the_existing_entries`.
        {
            let boot = tempfile::tempdir().unwrap();
            fs::write(boot.path().join("vmlinuz-6.1"), b"k").unwrap();
            // A separate /boot (or the ESP there): paths relative to it.
            assert_eq!(
                path_prefix(boot.path(), &["linux /vmlinuz-6.1\n".into()]),
                ""
            );
            // /boot on the root filesystem: absolute paths.
            assert_eq!(
                path_prefix(boot.path(), &["title x\nlinux /boot/vmlinuz-6.1\n".into()]),
                "/boot"
            );
            assert_eq!(path_prefix(boot.path(), &[]), "");
        }
        // Phase `the_running_entry_is_found_by_its_boot_image`.
        {
            let boot = tempfile::tempdir().unwrap();
            fs::write(boot.path().join("vmlinuz-6.19"), b"k").unwrap();
            let entries = [
                (
                    "b208-6.19".to_string(),
                    "linux /boot/vmlinuz-6.19\n".to_string(),
                ),
                (
                    "tilo-vm-7.2-aaaa".into(),
                    "linux /boot/tilo-vm/x/vmlinuz\n".into(),
                ),
            ];
            assert_eq!(
                running_entry(
                    "BOOT_IMAGE=(hd0,gpt3)/boot/vmlinuz-6.19 ro root=UUID=1",
                    &entries,
                    boot.path()
                ),
                Some("b208-6.19".into())
            );
            // A separate /boot: the loader's path has no /boot prefix.
            assert_eq!(
                running_entry("BOOT_IMAGE=/vmlinuz-6.19 ro", &entries, boot.path()),
                Some("b208-6.19".into())
            );
            assert_eq!(running_entry("ro root=UUID=1", &entries, boot.path()), None);
        }
        // Phase `only_the_new_entry_and_the_fallback_survive`.
        {
            let existing = [
                "fedora-6.1".to_string(),
                "tilo-vm-7.2+dev-aaaa".into(),
                "tilo-vm-7.2+dev-bbbb".into(),
                "tilo-vm-7.2+dev-cccc".into(),
            ];
            assert_eq!(
                stale_stems(&existing, &["tilo-vm-7.2+dev-cccc", "tilo-vm-7.2+dev-bbbb"]),
                ["tilo-vm-7.2+dev-aaaa"]
            );
            assert_eq!(
                stale_stems(&existing, &["tilo-vm-7.2+dev-cccc", "fedora-6.1"]).len(),
                2
            );
        }
        // Phase `the_image_dracut_arguments_are_complete_pairs`.
        {
            // Every option that takes a value has one, and the value is no option.
            for pair in IMAGE_DRACUT_ARGS.windows(2) {
                if ["--compress", "--omit", "--omit-drivers", "--add-drivers", "--add"].contains(&pair[0]) {
                    assert!(!pair[1].starts_with("--"), "{pair:?}");
                }
            }
            assert!(SYSEXT_DROPIN_TEXT.contains("udevadm trigger --action=add"));
            assert_eq!(KERNEL_LAYER_DRACUT_ARGS, ["--force-drivers", "overlay"]);
        }
    }

    #[test]
    fn boot_loaders_are_identified_and_name_entries_their_way() {
        // Phase `loader_info_names_the_loader`.
        {
            let var = |text: &str| {
                let mut bytes = vec![7, 0, 0, 0];
                for unit in text.encode_utf16().chain([0]) {
                    bytes.extend(unit.to_le_bytes());
                }
                bytes
            };
            assert_eq!(
                efivar_string(&var("systemd-boot 258.1")),
                "systemd-boot 258.1"
            );
            assert_eq!(efivar_string(&var("GRUB 2.12")), "GRUB 2.12");
            assert_eq!(efivar_string(&[7, 0]), "");
        }
        // Phase `loaders_name_entries_their_way`.
        {
            assert_eq!(Loader::SystemdBoot.entry_id("tilo-vm-a"), "tilo-vm-a.conf");
            assert_eq!(Loader::Grub.entry_id("tilo-vm-a"), "tilo-vm-a");
            assert_eq!(Loader::stem("tilo-vm-a.conf"), "tilo-vm-a");
            assert_eq!(
                systemd_boot_default(
                    r#"[{"id":"a.conf","isDefault":false},{"id":"b.conf","isDefault":true}]"#
                ),
                Some("b.conf".into())
            );
            assert_eq!(
                grub_default("saved_entry=abc-6.1\nboot_success=1\n"),
                Some("abc-6.1".into())
            );
            assert_eq!(grub_default("next_entry=\n"), None);
        }
    }
}
